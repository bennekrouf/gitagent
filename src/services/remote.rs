//! Running a command on another machine, over the system `ssh`.
//!
//! GitAgent stores no keys and knows nothing about your credentials. It builds
//! an `ssh` command line and lets the binary you already trust do the work, so
//! `~/.ssh/config` host aliases, agent forwarding, jump hosts and `known_hosts`
//! all behave exactly as they do in a terminal.
//!
//! Two options are always passed, and both matter:
//!
//! * `BatchMode=yes` — a subprocess has no terminal, so a password or
//!   passphrase prompt would hang forever with nothing on screen. Batch mode
//!   turns that into an immediate, readable failure instead.
//! * `ConnectTimeout` — an unreachable host should fail in seconds, not block
//!   a run indefinitely.
//!
//! Host key checking is deliberately left at its default. Accepting an unknown
//! host key is a security decision, and it is not one an app should make for
//! you behind a button.

use super::git;

/// The full `ssh` argument list. Split out from the call so it can be tested:
/// this is where a bug would silently send a command to the wrong host.
pub fn ssh_args(host: &str, port: &str, identity: &str, command: &str) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
    ];

    if !port.trim().is_empty() && safe_argument(port) {
        args.push("-p".into());
        args.push(port.trim().to_string());
    }
    if !identity.trim().is_empty() && safe_argument(identity) {
        args.push("-i".into());
        args.push(expand_home(identity.trim()));
    }

    // `--` first, then the host. Without it a host that begins with a dash
    // is read by ssh as an option rather than a destination, and
    // `-oProxyCommand=…` in that position runs an arbitrary command on this
    // machine. The same goes for a port or an identity path, which is why
    // both are rejected outright above rather than quoted.
    args.push("--".into());
    args.push(host.trim().to_string());
    if !command.trim().is_empty() {
        args.push(remote_command(command.trim()));
    }
    args
}

/// Whether a configured value can be handed to `ssh` as an argument.
///
/// A leading dash turns a destination or a path into an option. Nothing
/// legitimate here starts with one, so this is a flat refusal rather than an
/// escaping problem to be solved.
fn safe_argument(value: &str) -> bool {
    let value = value.trim();
    !value.starts_with('-')
}

/// What actually reaches the remote shell for one command.
///
/// `ssh host cmd` hands `cmd` to a *non-interactive* remote shell, which
/// never sources `~/.bashrc` — so a deploy command defined there as a bash
/// alias (a common pattern: `alias deploy-foo='cd ... && git pull && ...'`)
/// comes back "command not found" even though typing the same thing at an
/// interactive prompt on that host works fine. Wrapping in `bash -ic` forces
/// an interactive-enough shell that aliases actually resolve; `-l` (login)
/// was tried first and does not — most hosts do not source `.bashrc` from
/// their login files, only from interactive-shell startup.
fn remote_command(command: &str) -> String {
    format!("bash -ic {}", shell_quote(command))
}

/// Wraps `s` in single quotes so it reaches the remote shell as one
/// argument, verbatim — the standard `'\''` trick for a literal `'` inside
/// a single-quoted string.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `bash -ic` prints two harmless lines to stderr on every call — ssh gives
/// it no controlling terminal, so it cannot set up job control, and says so.
/// Nothing failed; stripping them keeps the step's log about the command
/// that actually ran.
fn strip_interactive_shell_noise(output: &str) -> String {
    output
        .lines()
        .filter(|line| {
            !line.contains("cannot set terminal process group")
                && *line != "bash: no job control in this shell"
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `~` is a shell convenience, and there is no shell here.
pub fn expand_home(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()
            .map(|home| home.join(rest).to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string()),
        None => path.to_string(),
    }
}

pub async fn run(
    host: &str,
    port: &str,
    identity: &str,
    command: &str,
    stdin: &str,
) -> (bool, String) {
    run_streaming(host, port, identity, command, stdin, &mut |_| {}).await
}

/// `run`, but reporting each line of the remote command's output as it
/// arrives — what lets a step's log fill in live instead of appearing all at
/// once when the connection closes.
pub async fn run_streaming(
    host: &str,
    port: &str,
    identity: &str,
    command: &str,
    stdin: &str,
    on_line: &mut dyn FnMut(&str),
) -> (bool, String) {
    if !safe_argument(host) {
        return (
            false,
            format!(
                "refusing to connect to {host:?}: a host beginning with `-` is read by ssh \
                 as an option, not a destination"
            ),
        );
    }
    let args = ssh_args(host, port, identity, command);
    let (ok, output) = git::run_streaming("ssh", &args, None, stdin, on_line).await;
    (ok, strip_interactive_shell_noise(&output))
}

/// Proves the host answers and the credentials work, without running anything
/// of consequence on it.
pub async fn test(host: &str, port: &str, identity: &str) -> Result<String, String> {
    if host.trim().is_empty() {
        return Err("No host set".into());
    }
    let (ok, output) = run(host, port, identity, "echo gitagent-ok && uname -sn", "").await;

    let text = output.replace("gitagent-ok", "").trim().to_string();
    if ok {
        Ok(if text.is_empty() {
            format!("{host} answered")
        } else {
            format!("{host} — {text}")
        })
    } else {
        Err(if output.trim().is_empty() {
            format!("could not reach {host}")
        } else {
            output
        })
    }
}

/// Private keys sitting in `~/.ssh`, newest first — what Setup offers as
/// one-click suggestions for the "Identity file" field, so picking a key does
/// not mean knowing its filename or its path by heart.
///
/// A file only counts as a private key when a `<name>.pub` sibling exists
/// next to it: that is the actual signal that separates a keypair's private
/// half from `config`, `known_hosts`, `authorized_keys`, and everything else
/// that also lives in `~/.ssh`. The `.pub` file itself is never offered —
/// pointing `ssh -i` at the public half is a common mistake this sidesteps.
pub fn discover_identities() -> Vec<String> {
    let Some(home) = dirs::home_dir() else {
        return vec![];
    };
    identities_in(&home.join(".ssh"))
        .into_iter()
        .map(|name| format!("~/.ssh/{name}"))
        .collect()
}

/// The scanning logic behind `discover_identities`, taking the directory
/// directly so tests do not have to touch the real `~/.ssh`.
fn identities_in(ssh_dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(ssh_dir) else {
        return vec![];
    };

    let mut found: Vec<(std::time::SystemTime, String)> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            !name.ends_with(".pub") && ssh_dir.join(format!("{name}.pub")).is_file()
        })
        .filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?;
            Some((modified, e.file_name().to_string_lossy().to_string()))
        })
        .collect();

    found.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    found.into_iter().map(|(_, name)| name).collect()
}

#[cfg(test)]
mod tests {

    #[tokio::test]
    async fn a_host_beginning_with_a_dash_is_refused_rather_than_passed_to_ssh() {
        // `-oProxyCommand=...` in the destination slot is local command
        // execution, so this is a refusal, not an escaping problem.
        let (ok, why) = run("-oProxyCommand=touch /tmp/pwned", "", "", "true", "").await;
        assert!(!ok);
        assert!(why.contains("refusing to connect"), "got {why:?}");
    }

    #[test]
    fn the_host_is_separated_from_the_options_by_a_double_dash() {
        let args = ssh_args("example.com", "", "", "uptime");
        let dashdash = args.iter().position(|a| a == "--").expect("no `--`");
        let host = args.iter().position(|a| a == "example.com").unwrap();
        assert!(dashdash < host, "got {args:?}");
    }

    #[test]
    fn an_option_shaped_port_or_identity_is_dropped_not_forwarded() {
        let args = ssh_args("example.com", "-oProxyCommand=x", "-oProxyCommand=y", "uptime");
        assert!(
            !args.iter().any(|a| a.contains("ProxyCommand")),
            "got {args:?}"
        );
    }
    use super::*;
    use std::io::Write;

    fn temp_ssh_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gitagent-test-ssh-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(dir: &std::path::Path, name: &str) {
        std::fs::File::create(dir.join(name))
            .unwrap()
            .write_all(b"x")
            .unwrap();
    }

    #[test]
    fn only_a_private_key_with_a_matching_pub_sibling_is_offered() {
        let dir = temp_ssh_dir();
        touch(&dir, "id_ed25519");
        touch(&dir, "id_ed25519.pub");
        touch(&dir, "config");
        touch(&dir, "known_hosts");
        touch(&dir, "orphan_key"); // no .pub sibling — not a confirmed keypair

        let found = identities_in(&dir);
        assert_eq!(found, vec!["id_ed25519".to_string()]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_public_half_is_never_offered_on_its_own() {
        let dir = temp_ssh_dir();
        touch(&dir, "id_rsa");
        touch(&dir, "id_rsa.pub");

        let found = identities_in(&dir);
        assert!(!found.iter().any(|f| f.ends_with(".pub")));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_ssh_directory_yields_no_identities_rather_than_an_error() {
        let dir = std::env::temp_dir().join("gitagent-test-ssh-does-not-exist");
        assert_eq!(identities_in(&dir), Vec::<String>::new());
    }

    #[test]
    fn a_prompt_can_never_hang_the_run() {
        let args = ssh_args("build@ci.example.com", "", "", "uptime");
        assert!(args.windows(2).any(|w| w == ["-o", "BatchMode=yes"]));
    }

    #[test]
    fn an_unreachable_host_fails_rather_than_blocking() {
        let args = ssh_args("h", "", "", "true");
        assert!(args.windows(2).any(|w| w == ["-o", "ConnectTimeout=10"]));
    }

    #[test]
    fn the_host_comes_before_the_command() {
        let args = ssh_args("build@ci.example.com", "", "", "./deploy.sh");
        let host = args
            .iter()
            .position(|a| a == "build@ci.example.com")
            .unwrap();
        let cmd = args.iter().position(|a| a.contains("./deploy.sh")).unwrap();
        assert!(host < cmd, "ssh takes the destination first");
    }

    #[test]
    fn the_command_stays_one_argument() {
        // Splitting it would make ssh reassemble it differently and quoting
        // would stop meaning what it says.
        let args = ssh_args("h", "", "", "cd /srv && ./release.sh --patch");
        assert!(args
            .iter()
            .any(|a| a.contains("cd /srv && ./release.sh --patch")));
    }

    #[test]
    fn the_command_is_wrapped_so_bashrc_aliases_resolve() {
        // A remote alias like `deploy-foo` (defined in ~/.bashrc) is only
        // visible to an interactive shell — plain `ssh host cmd` would say
        // "command not found" for it even though it works at the prompt.
        let args = ssh_args("h", "", "", "deploy-mayorana");
        let cmd = args.last().unwrap();
        assert_eq!(cmd, "bash -ic 'deploy-mayorana'");
    }

    #[test]
    fn a_single_quote_in_the_command_cannot_break_out_of_the_wrapper() {
        let args = ssh_args("h", "", "", "echo 'hi there'");
        let cmd = args.last().unwrap();
        assert_eq!(cmd, r"bash -ic 'echo '\''hi there'\'''");
    }

    #[test]
    fn an_ssh_config_alias_works_as_a_host() {
        let args = ssh_args("prod-web", "", "", "true");
        assert!(args.contains(&"prod-web".to_string()));
        assert!(
            !args.iter().any(|a| a == "-i"),
            "no key needed for an alias"
        );
    }

    #[test]
    fn optional_settings_are_left_out_when_empty() {
        let args = ssh_args("h", "", "", "true");
        assert!(!args.iter().any(|a| a == "-p"));
        assert!(!args.iter().any(|a| a == "-i"));
    }

    #[test]
    fn a_port_and_an_identity_are_passed_through() {
        let args = ssh_args("h", "2222", "/keys/id_ed25519", "true");
        assert!(args.windows(2).any(|w| w == ["-p", "2222"]));
        assert!(args.windows(2).any(|w| w == ["-i", "/keys/id_ed25519"]));
    }

    #[test]
    fn whitespace_around_a_setting_does_not_reach_ssh() {
        let args = ssh_args("  h  ", " 22 ", "", "  true  ");
        assert!(args.contains(&"h".to_string()));
        assert!(args.windows(2).any(|w| w == ["-p", "22"]));
        assert!(args.iter().any(|a| a == "bash -ic 'true'"));
    }

    #[test]
    fn a_tilde_in_a_key_path_is_expanded_because_there_is_no_shell() {
        let expanded = expand_home("~/.ssh/id_ed25519");
        assert!(!expanded.starts_with('~'));
        assert!(expanded.ends_with("/.ssh/id_ed25519"));
    }

    #[test]
    fn an_absolute_key_path_is_untouched() {
        assert_eq!(expand_home("/etc/keys/id"), "/etc/keys/id");
    }

    #[test]
    fn host_key_checking_is_never_disabled() {
        // Accepting an unknown host key is the user's call, in a terminal.
        let args = ssh_args("h", "", "", "true").join(" ");
        assert!(!args.contains("StrictHostKeyChecking"));
        assert!(!args.contains("UserKnownHostsFile"));
    }

    #[test]
    fn the_job_control_warning_is_stripped_but_real_output_is_kept() {
        let raw = "bash: cannot set terminal process group (-1): Inappropriate ioctl for device\n\
                   bash: no job control in this shell\n\
                   ✅ mayorana deployed";
        assert_eq!(strip_interactive_shell_noise(raw), "✅ mayorana deployed");
    }

    #[test]
    fn a_line_that_only_mentions_job_control_in_its_own_output_survives() {
        // The exact two known-benign lines are filtered by content, not by
        // merely containing "bash:" — a script legitimately printing its own
        // "bash: ..." diagnostic must not be silently eaten.
        let raw = "bash: my-script.sh: command not found";
        assert_eq!(strip_interactive_shell_noise(raw), raw);
    }
}
