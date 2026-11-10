//! Which hosting platform a repository lives on, and what it needs from you
//! before a run can finish.
//!
//! Pull requests are the one part of the flow that is not plain git: GitHub
//! wants `gh pr create`, Azure DevOps wants `az repos pr create`, and each has
//! its own idea of authentication. Detecting the forge from the remote URL up
//! front means the run fails in the first node with an actionable message,
//! rather than in the last one after it has already committed and pushed.

use super::git;
use super::graph::Remedy;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Forge {
    GitHub,
    AzureDevOps,
    /// A remote we can push to but cannot open a pull request on.
    Unsupported(String),
    /// No `origin` at all.
    None,
}

impl Forge {
    pub fn label(&self) -> String {
        match self {
            Forge::GitHub => "GitHub".into(),
            Forge::AzureDevOps => "Azure DevOps".into(),
            Forge::Unsupported(host) => format!("unsupported ({host})"),
            Forge::None => "no remote".into(),
        }
    }

    pub fn as_key(&self) -> String {
        match self {
            Forge::GitHub => "github".into(),
            Forge::AzureDevOps => "azure".into(),
            Forge::Unsupported(h) => format!("unsupported:{h}"),
            Forge::None => "none".into(),
        }
    }

    pub fn from_key(key: &str) -> Forge {
        match key {
            "github" => Forge::GitHub,
            "azure" => Forge::AzureDevOps,
            "none" => Forge::None,
            other => Forge::Unsupported(other.trim_start_matches("unsupported:").to_string()),
        }
    }
}

/// Classifies a remote URL. Handles both SSH (`git@host:path`,
/// `ssh://git@host/path`) and HTTPS forms.
pub fn detect(remote_url: &str) -> Forge {
    let url = remote_url.trim();
    if url.is_empty() {
        return Forge::None;
    }
    let lower = url.to_lowercase();

    if lower.contains("github.com") {
        return Forge::GitHub;
    }
    // Azure DevOps has three live URL shapes: the modern dev.azure.com, the
    // legacy {org}.visualstudio.com, and the SSH host ssh.dev.azure.com.
    if lower.contains("dev.azure.com") || lower.contains("visualstudio.com") {
        return Forge::AzureDevOps;
    }

    Forge::Unsupported(host_of(&lower))
}

/// What `host_of` calls a remote that is a folder on this machine.
const LOCAL: &str = "local folder";

/// Whether a remote is a path on disk rather than a server: `C:\\...`,
/// `C:/...`, a UNC share, an absolute or relative Unix path, or `file://`.
/// Checked before scp-style `host:path` parsing, which would otherwise read
/// a Windows drive letter as the host.
fn is_local_path(url: &str) -> bool {
    let b = url.as_bytes();
    let drive =
        b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/');
    drive || url.starts_with("file://") || url.starts_with(['/', '\\', '.', '~'])
}

/// Best-effort host extraction, for the "unsupported" message only.
fn host_of(url: &str) -> String {
    if is_local_path(url) {
        return LOCAL.into();
    }
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    // SSH short form: git@host:org/repo
    let after_user = after_scheme
        .split_once('@')
        .map(|(_, rest)| rest)
        .unwrap_or(after_scheme);
    after_user
        .split(['/', ':'])
        .next()
        .unwrap_or("unknown")
        .to_string()
}

#[derive(Clone, PartialEq, Debug)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    /// Present only when the failure has an exact, non-interactive fix.
    pub fix: Option<Remedy>,
    /// What to do about a failure the app cannot fix itself, step by step.
    /// Empty when the detail line already says it all.
    pub help: String,
}

impl Check {
    fn pass(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ok: true,
            detail: detail.into(),
            fix: None,
            help: String::new(),
        }
    }
    fn fail(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ok: false,
            detail: detail.into(),
            fix: None,
            help: String::new(),
        }
    }

    /// A failure the app can repair itself, given a button press.
    fn fixable(name: &str, detail: impl Into<String>, fix: Remedy) -> Self {
        Self {
            name: name.into(),
            ok: false,
            detail: detail.into(),
            fix: Some(fix),
            help: String::new(),
        }
    }

    fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = help.into();
        self
    }
}

/// A forge CLI that could not be started, with an install button where the
/// platform has a standard one-line install.
fn missing_cli(name: &str, program: &str) -> Check {
    let detail = "not installed, or not on the PATH GitAgent can see — install it, then refresh";
    match install_remedy(program) {
        Some(fix) => Check::fixable(name, detail, fix),
        None => Check::fail(name, detail),
    }
}

fn install_remedy(program: &str) -> Option<Remedy> {
    let (cmd, args) = git::install_command(program)?;
    Some(Remedy::new(&format!("Install {program}"), cmd, &args))
}

/// `az login` opens the browser to sign in and returns once that is done, so
/// unlike `gh auth login` (which has to show a code and take an answer) it
/// can run from a button. Without a browser it prints a device-code link
/// instead, which the remedy's output shows.
fn az_login_remedy() -> Remedy {
    Remedy::new("Sign in with az login", "az", &["login"])
}

/// A connection to Azure DevOps that failed below the sign-in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AzureConnection {
    Certificate,
    Proxy,
}

/// Which failure `az` output shows, if any. Only ever classified: `--debug`
/// output can carry credentials, so none of its text is kept or shown.
fn azure_connection_failure(output: &str) -> Option<AzureConnection> {
    let lower = output.to_lowercase();
    let has = |markers: &[&str]| markers.iter().any(|m| lower.contains(m));
    if has(&[
        "certificate_verify_failed",
        "certificate verify failed",
        "sslcertverificationerror",
        "unable to get local issuer certificate",
        "self signed certificate",
        "self-signed certificate",
    ]) {
        return Some(AzureConnection::Certificate);
    }
    if has(&[
        "proxyerror",
        "tunnel connection failed",
        "proxy authentication required",
    ]) {
        return Some(AzureConnection::Proxy);
    }
    None
}

/// The preflight line for a connection that failed, with what to do about it.
fn azure_connection_check(failure: AzureConnection) -> Check {
    match failure {
        AzureConnection::Certificate => Check::fail(
            "azure devops auth",
            "your Azure sign-in works, but the connection to Azure DevOps failed \
             certificate verification — usually a proxy that inspects TLS",
        )
        .with_help(
            "The Azure CLI uses its own list of trusted certificates, which does not\n\
             include your proxy's root certificate.\n\
             \n\
             1. Get the proxy's root certificate from your IT team, in PEM format.\n\
             2. Make a bundle: a copy of the CLI's cacert.pem with that root added.\n\
             3. Set the user environment variable REQUESTS_CA_BUNDLE to the bundle's\n\
             \x20  full path.\n\
             4. Run this step again.",
        ),
        AzureConnection::Proxy => Check::fail(
            "azure devops auth",
            "your Azure sign-in works, but the proxy refused the connection to Azure DevOps",
        )
        .with_help(
            "Check that HTTPS_PROXY is set to your proxy, or empty if none is needed,\n\
             and that the proxy allows dev.azure.com. Your IT team can confirm both.\n\
             Then run this step again.",
        ),
    }
}

/// What a generic "run the login command" from `az` really means. The
/// azure-devops extension says that for a certificate failure too, so when a
/// token is available `debug` (a `--debug` rerun) is looked at for the cause.
fn azure_sign_in_check(token_available: bool, debug: &str) -> Check {
    match azure_connection_failure(debug).filter(|_| token_available) {
        Some(failure) => azure_connection_check(failure),
        None => Check::fixable(
            "azure devops auth",
            "not signed in to Azure DevOps — sign in again with `az login`, \
             or run `az devops login` with a PAT in a terminal",
            az_login_remedy(),
        ),
    }
}

/// Whether Azure will issue a token for Azure DevOps. Asks only for its
/// expiry, so the token itself never reaches this process.
async fn azure_token_available() -> bool {
    git::run(
        ".",
        "az",
        &[
            "account",
            "get-access-token",
            "--resource",
            AZURE_DEVOPS_RESOURCE,
            "--query",
            "expiresOn",
            "-o",
            "tsv",
        ],
    )
    .await
    .is_ok()
}

/// Azure DevOps' application id in Microsoft Entra.
const AZURE_DEVOPS_RESOURCE: &str = "499b84ac-1321-427f-aa17-267ca6975798";

/// A fix for why the open pull requests could not be listed, when the app
/// has one: installing the CLI, signing in to Azure, or adding the
/// azure-devops extension. Matched on the error text, since that is all a
/// failed CLI call leaves.
pub fn pr_list_remedy(forge: &Forge, err: &str) -> Option<Remedy> {
    let program = match forge {
        Forge::GitHub => "gh",
        Forge::AzureDevOps => "az",
        _ => return None,
    };
    if err.contains("not on the PATH GitAgent can see") {
        return install_remedy(program);
    }
    if program != "az" {
        return None;
    }
    if azure_connection_failure(err).is_some() {
        return None;
    }
    let lower = err.to_lowercase();
    if lower.contains("az login") || lower.contains("login command") || lower.contains("aadsts") {
        return Some(az_login_remedy());
    }
    if lower.contains("'repos' is misspelled") || lower.contains("azure-devops") {
        return Some(Remedy::new(
            "Add the azure-devops extension",
            "az",
            &["extension", "add", "--name", "azure-devops"],
        ));
    }
    None
}

/// How to sign the GitHub CLI in, for someone who has never done it. `said`
/// is what `gh auth status` answered: it fails when *any* account it knows
/// has a stale token, even with another one signed in, and only its own
/// words tell those two cases apart.
fn gh_login_steps(said: &str) -> String {
    let said = said
        .split("\n\nwhile running:")
        .next()
        .unwrap_or_default()
        .trim();
    let mut steps = String::from(
        "To sign the GitHub CLI in:\n\
         1. Open a terminal and run:  gh auth login\n\
         2. Answer its questions: GitHub.com; SSH if your remote starts with\n\
         \x20  git@, HTTPS otherwise; then \"Login with a web browser\".\n\
         3. Copy the one-time code it shows, press Enter, paste the code in the\n\
         \x20  browser page that opens and authorize GitHub CLI.\n\
         4. Check it with:  gh auth status  — it should say \"Logged in to\n\
         \x20  github.com\" and report no errors.\n\
         5. Run the flow again. No restart needed.\n\
         \n\
         Or, instead: create a personal access token with the repo scope at\n\
         github.com/settings/tokens, export GH_TOKEN=<token> in your shell\n\
         profile (~/.zshrc), and restart GitAgent so it reads it.",
    );
    if said.contains("Logged in") {
        steps.push_str(
            "\n\nOne of your accounts is signed in, but gh reports a problem with\n\
             another. Run  gh auth logout  for the stale one, or\n\
             gh auth refresh, until  gh auth status  shows no errors.",
        );
    }
    if !said.is_empty() {
        steps.push_str(&format!("\n\ngh auth status said:\n{said}"));
    }
    steps
}

/// The organization's address, `https://dev.azure.com/{org}`, from any of
/// the three remote shapes. Empty for a remote that is none of them.
fn azure_org_url(remote: &str) -> String {
    let remote = remote.trim();
    if let Some(rest) = remote.split("ssh.dev.azure.com:v3/").nth(1) {
        return format!(
            "https://dev.azure.com/{}",
            rest.split('/').next().unwrap_or_default()
        );
    }
    if let Some(rest) = remote.split("dev.azure.com/").nth(1) {
        return format!(
            "https://dev.azure.com/{}",
            rest.split('/').next().unwrap_or_default()
        );
    }
    if let Some(host) = remote
        .split("://")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .filter(|h| h.ends_with(".visualstudio.com"))
    {
        let org = host.trim_end_matches(".visualstudio.com");
        let org = org.rsplit('@').next().unwrap_or(org);
        return format!("https://dev.azure.com/{org}");
    }
    String::new()
}

/// Organization URL, project and repository name from an Azure DevOps remote,
/// with `%20` and the like decoded. `None` for any other remote.
fn azure_coordinates(remote: &str) -> Option<(String, String, String)> {
    let org = azure_org_url(remote);
    if org.is_empty() {
        return None;
    }
    let remote = remote.trim().trim_end_matches('/');
    let path: Vec<&str> = if let Some(rest) = remote.split("ssh.dev.azure.com:v3/").nth(1) {
        // org/project/repo
        rest.split('/').skip(1).collect()
    } else if let Some(rest) = remote.split("dev.azure.com/").nth(1) {
        // org/project/_git/repo
        rest.split('/').skip(1).filter(|s| *s != "_git").collect()
    } else {
        // {org}.visualstudio.com/[DefaultCollection/]project/_git/repo
        let rest = remote.split("://").nth(1)?;
        rest.split('/')
            .skip(1)
            .filter(|s| *s != "_git" && *s != "DefaultCollection")
            .collect()
    };
    match path.as_slice() {
        [project, repo] if !project.is_empty() && !repo.is_empty() => Some((
            org,
            percent_decode(project),
            percent_decode(repo.trim_end_matches(".git")),
        )),
        _ => None,
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Some(b) = text
                .get(i + 1..i + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
            {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The arguments that tell `az repos …` which organization, project and
/// repository `origin` points at. Left to itself, az finds them by running
/// git on its own, and on Windows that can fail even inside the repository
/// ("no Azure DevOps remote was found … --organization must be specified").
/// Each command accepts only some of them, hence `Scope`. Empty when the
/// remote cannot be read, so az falls back to detecting them itself.
pub async fn azure_scope(repo: &str, scope: Scope) -> Vec<String> {
    let remote = git::run(repo, "git", &["remote", "get-url", "origin"])
        .await
        .unwrap_or_default();
    azure_scope_for(&remote, scope)
}

/// How much of the remote an `az` command takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    /// `az repos pr show` and `update`: the pull request id says the rest.
    Organization,
    /// `az repos list`.
    Project,
    /// `az repos pr list` and `create`.
    Repository,
}

fn azure_scope_for(remote: &str, scope: Scope) -> Vec<String> {
    let Some((org, project, name)) = azure_coordinates(remote) else {
        return vec![];
    };
    let mut args = vec!["--organization".to_string(), org];
    if scope != Scope::Organization {
        args.extend(["--project".into(), project]);
    }
    if scope == Scope::Repository {
        args.extend(["--repository".into(), name]);
    }
    args.extend(["--detect".into(), "false".into()]);
    args
}

/// What to do when `az login` is fine but Azure DevOps turns it away. The
/// usual reason is the tenant: an account in several directories signs in
/// to whichever `az` picked as default, often a customer's, and the
/// organization does not know you there. The tenant that is right may
/// demand multi-factor authentication first, which `az login` reports
/// only as AADSTS50079.
fn azure_access_steps(org: &str) -> String {
    let org = if org.is_empty() {
        "https://dev.azure.com/<organization>"
    } else {
        org
    };
    format!(
        "You are signed in to Azure, but Azure DevOps does not accept that sign-in\n\
         for this organization. Usually az signed in to another tenant (directory)\n\
         than the one the organization is connected to.\n\
         \n\
         1. Open {org} in your browser with the same account. If you cannot\n\
         \x20  see the project, ask an Azure DevOps admin to add you; nothing\n\
         \x20  else will help until they do.\n\
         2. Find the organization's tenant: Organization settings, then\n\
         \x20  Microsoft Entra.\n\
         3. In a terminal, run:\n\
         \x20  az login --tenant <that tenant ID> --allow-no-subscriptions\n\
         \x20  If it says AADSTS50079, that tenant wants multi-factor\n\
         \x20  authentication: enroll at https://aka.ms/mfasetup, or ask your\n\
         \x20  IT if it will not let you, then run az login again.\n\
         4. Check it from the repository folder with:  az repos list -o table\n\
         \x20  When it lists the repositories, run the flow again.\n\
         \n\
         Or, instead: create a personal access token with the Code (Read &\n\
         Write) scope under User settings, Personal access tokens, in\n\
         {org}. Export AZURE_DEVOPS_EXT_PAT=<token> in your shell profile\n\
         (~/.zshrc) and restart GitAgent so it reads it. A token does not\n\
         depend on the tenant az signed in to."
    )
}

/// Everything that must be true before a run can reach `open_pr`.
///
/// On GitHub, `gh auth status` asks GitHub about the token, and every flow's
/// preflight asked again. A pass is kept for half an hour; a failure never
/// is, so fixing it shows at once.
pub async fn check_credentials(forge: &Forge, repo: &str) -> Vec<Check> {
    type Kept = Option<(std::time::Instant, Vec<Check>)>;
    static PASSED: std::sync::Mutex<Kept> = std::sync::Mutex::new(None);
    const KEEP: std::time::Duration = std::time::Duration::from_secs(30 * 60);
    if *forge != Forge::GitHub {
        return checked_credentials(forge, repo).await;
    }
    if let Some((at, checks)) = PASSED.lock().ok().and_then(|g| g.clone()) {
        if at.elapsed() < KEEP {
            return checks;
        }
    }
    let checks = checked_credentials(forge, repo).await;
    if checks.iter().all(|c| c.ok) {
        if let Ok(mut g) = PASSED.lock() {
            *g = Some((std::time::Instant::now(), checks.clone()));
        }
    }
    checks
}

async fn checked_credentials(forge: &Forge, repo: &str) -> Vec<Check> {
    match forge {
        Forge::GitHub => {
            if !git::has_gh().await {
                return vec![missing_cli("gh CLI", "gh")];
            }
            match git::run(".", "gh", &["auth", "status"]).await {
                Ok(out) => vec![Check::pass(
                    "gh auth",
                    out.lines()
                        .find(|l| l.contains("Logged in"))
                        .unwrap_or("authenticated")
                        .trim(),
                )],
                // Deliberately not fixable in-app: `gh auth login` needs a
                // terminal to show the device code and take the answer. So
                // the steps are spelled out instead.
                Err(e) => vec![Check::fail(
                    "gh auth",
                    "not authenticated — run `gh auth login`, or set GH_TOKEN",
                )
                .with_help(gh_login_steps(&e))],
            }
        }
        Forge::AzureDevOps => {
            let mut checks = vec![];
            match git::run(".", "az", &["version"]).await {
                Ok(_) => checks.push(Check::pass("az CLI", "installed")),
                Err(_) => {
                    checks.push(missing_cli("az CLI", "az"));
                    return checks;
                }
            }
            match git::run(".", "az", &["extension", "show", "--name", "azure-devops"]).await {
                Ok(_) => checks.push(Check::pass("azure-devops extension", "installed")),
                Err(_) => checks.push(Check::fixable(
                    "azure-devops extension",
                    "missing",
                    Remedy::new(
                        "Add the azure-devops extension",
                        "az",
                        &["extension", "add", "--name", "azure-devops"],
                    ),
                )),
            }
            match git::run(
                ".",
                "az",
                &["account", "show", "--query", "user.name", "-o", "tsv"],
            )
            .await
            {
                Ok(user) => checks.push(Check::pass("az login", user.trim())),
                Err(_) => checks.push(Check::fixable(
                    "az login",
                    "not signed in — sign in with `az login`, or set AZURE_DEVOPS_EXT_PAT",
                    az_login_remedy(),
                )),
            }

            // `az account show` only proves an AAD sign-in. Azure DevOps is a
            // separate credential, and `az repos pr create` fails on it at the
            // very end of a run — after a branch and a push. This exercises the
            // same path (including org/project detection from the remote), so
            // the failure lands here instead.
            let scope = azure_scope(repo, Scope::Project).await;
            let mut args = vec!["repos", "list", "--output", "none"];
            args.extend(scope.iter().map(String::as_str));
            match git::run(repo, "az", &args).await {
                Ok(_) => checks.push(Check::pass("azure devops auth", "can reach the project")),
                Err(e) if e.contains("you need to run the login command") => {
                    let token_available = azure_token_available().await;
                    let debug = if token_available {
                        let mut with_debug = args.clone();
                        with_debug.push("--debug");
                        git::run(repo, "az", &with_debug).await.err()
                    } else {
                        None
                    };
                    checks.push(azure_sign_in_check(
                        token_available,
                        debug.as_deref().unwrap_or_default(),
                    ))
                }
                Err(e) => {
                    if let Some(failure) = azure_connection_failure(&e) {
                        checks.push(azure_connection_check(failure));
                        return checks;
                    }
                    let remote = git::run(repo, "git", &["remote", "get-url", "origin"])
                        .await
                        .unwrap_or_default();
                    let check = Check::fail(
                        "azure devops auth",
                        e.lines()
                            .next()
                            .unwrap_or("could not reach the project")
                            .to_string(),
                    );
                    // Not authorized, with `az login` passing just above: the
                    // sign-in is real, but for a tenant this organization is
                    // not connected to, or one it does not know you in.
                    if e.contains("TF400813") || e.contains("not authorized") {
                        checks.push(check.with_help(azure_access_steps(&azure_org_url(&remote))))
                    } else {
                        checks.push(check)
                    }
                }
            }
            checks
        }
        // Not a failure: nothing here can be fixed, and every step up to and
        // including the push works without a forge. Only opening the pull
        // request does not, and that step says so when it gets there.
        Forge::Unsupported(host) => vec![Check::pass(
            "forge",
            if host == LOCAL {
                "origin is a local folder — everything up to and including the push works; \
                 pull requests need GitHub or Azure DevOps"
                    .to_string()
            } else {
                format!(
                    "{host} is not supported for pull requests — GitHub and Azure DevOps only. \
                     Everything up to and including the push still works."
                )
            },
        )],
        // Not a failure either: scanning, testing and committing need no
        // remote, and a run stopped here could do none of them. The push and
        // the pull request say so themselves when they get there.
        Forge::None => vec![Check::pass(
            "remote",
            "no `origin` remote — committing works; pushing and pull requests need one",
        )],
    }
}

/// Opens the pull request on whichever forge this repository lives on.
/// Returns the PR URL.
/// Whether a create failed only because the pull request is already there.
///
/// Both platforms say so in their own words; neither is an error worth
/// stopping a run over, because the outcome the step wanted already holds.
pub fn already_exists(error: &str) -> bool {
    let e = error.to_lowercase();
    e.contains("already exists")
        // Azure DevOps: "TF401179: An active pull request for the source and
        // target branch already exists."
        || e.contains("tf401179")
}

/// The pull request URL out of a "already exists" message, when it carries
/// one. `gh` does; `az` does not, so the caller falls back to looking it up.
pub fn url_in(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|word| word.starts_with("https://") && word.contains("/pull"))
        .map(|w| w.trim_end_matches(['.', ',', ')']).to_string())
}

/// The pull request a branch has, and whether it was already there.
#[derive(Clone, PartialEq, Debug)]
pub struct OpenedPr {
    pub url: String,
    pub already_open: bool,
}

impl OpenedPr {
    fn new(url: String) -> Self {
        Self {
            url,
            already_open: false,
        }
    }

    fn existing(url: String) -> Self {
        Self {
            url,
            already_open: true,
        }
    }
}

/// A description as `az repos pr create --description` takes it, one value a
/// line. On Windows `az` is a batch file, and Rust refuses to pass a line
/// break to one, so a multi-line description cannot be a single argument.
///
/// A line that argparse would read as an option (`---`, `-x`, `--a=b`) gets
/// one leading space, which markdown ignores. A bullet (`- item`) is left
/// alone: argparse takes it as text, and indenting it could re-nest a list.
fn azure_description_values(body: &str) -> Vec<String> {
    if body.trim().is_empty() {
        return vec![];
    }
    body.lines()
        .map(|line| {
            if line.starts_with('-') && !line.starts_with("- ") {
                format!(" {line}")
            } else {
                line.to_string()
            }
        })
        .collect()
}

/// A title on one line, whatever a model wrote: see `azure_description_values`.
fn azure_title(title: &str) -> String {
    title
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Everything after `az` that opens a pull request.
fn azure_create_args<'a>(
    head: &'a str,
    base: &'a str,
    title: &'a str,
    description: &'a [String],
    scope: &'a [String],
) -> Vec<&'a str> {
    let mut args = vec![
        "repos",
        "pr",
        "create",
        "--source-branch",
        head,
        "--target-branch",
        base,
        "--title",
        title,
    ];
    if !description.is_empty() {
        args.push("--description");
        args.extend(description.iter().map(String::as_str));
    }
    args.extend(["--output", "json"]);
    args.extend(scope.iter().map(String::as_str));
    args
}

pub async fn create_pr(
    forge: &Forge,
    repo: &str,
    base: &str,
    head: &str,
    title: &str,
    body: &str,
) -> Result<OpenedPr, String> {
    match forge {
        Forge::GitHub => match git::gh_pr_create(repo, base, head, title, body)
            .await
            .inspect(|_| git::forget_prs(repo))
        {
            Ok(url) => Ok(OpenedPr::new(url)),
            // Re-running a flow whose pull request already landed is not a
            // failure; adopt the existing one and carry on.
            Err(e) if already_exists(&e) => match url_in(&e) {
                Some(url) => Ok(OpenedPr::existing(url)),
                None => git::run(
                    repo,
                    "gh",
                    &["pr", "view", head, "--json", "url", "-q", ".url"],
                )
                .await
                .map(|out| OpenedPr::existing(out.trim().to_string()))
                .map_err(|_| e),
            },
            Err(e) => Err(e),
        },
        Forge::AzureDevOps => {
            let scope = azure_scope(repo, Scope::Repository).await;
            let title = azure_title(title);
            let description = azure_description_values(body);
            let args = azure_create_args(head, base, &title, &description, &scope);
            let out = git::run(repo, "az", &args).await;

            let out = match out {
                Ok(out) => out,
                Err(e) if already_exists(&e) => {
                    // az does not name the pull request in the error, so ask.
                    let mut args = vec![
                        "repos",
                        "pr",
                        "list",
                        "--source-branch",
                        head,
                        "--target-branch",
                        base,
                        "--status",
                        "active",
                        "--output",
                        "json",
                    ];
                    args.extend(scope.iter().map(String::as_str));
                    let listed = git::run(repo, "az", &args).await.map_err(|_| e.clone())?;

                    let first = serde_json::from_str::<serde_json::Value>(&listed)
                        .ok()
                        .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
                        .ok_or(e)?;
                    let remote = git::run(repo, "git", &["remote", "get-url", "origin"])
                        .await
                        .unwrap_or_default();
                    return Ok(OpenedPr::existing(
                        azure_pr_url(&first.to_string())
                            .or_else(|| azure_pr_url_from_remote(&first, &remote))
                            .unwrap_or_else(|| "pull request already open".into()),
                    ));
                }
                Err(e) => return Err(e),
            };
            Ok(OpenedPr::new(
                azure_pr_url(&out).unwrap_or_else(|| out.trim().to_string()),
            ))
        }
        Forge::Unsupported(host) => Err(format!(
            "cannot open a pull request on {host}. The branch is pushed — open it by hand."
        )),
        Forge::None => Err("no `origin` remote to open a pull request against".into()),
    }
}

/// `az repos pr create` returns the PR object; the browsable URL has to be
/// assembled from the repository web URL and the PR id.
fn azure_pr_url(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let id = value.get("pullRequestId")?.as_i64()?;
    let web = value.get("repository")?.get("webUrl")?.as_str()?;
    Some(format!("{web}/pullrequest/{id}"))
}

/// The link to a pull request worked out from `origin`, for a reply that has
/// no web address in it.
fn azure_pr_url_from_remote(pr: &serde_json::Value, remote: &str) -> Option<String> {
    let id = pr.get("pullRequestId")?.as_i64()?;
    let (org, project, name) = azure_coordinates(remote)?;
    let encoded = |text: &str| text.replace(' ', "%20");
    Some(format!(
        "{org}/{}/_git/{}/pullrequest/{id}",
        encoded(&project),
        encoded(&name)
    ))
}

#[cfg(test)]
mod tests;
