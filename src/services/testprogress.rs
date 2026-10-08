//! How far a test run has got, tier by tier, read from its output.
//!
//! The run-tests step runs one command and streams what it prints. Some
//! runners say enough in that stream to draw progress from: `cargo test`
//! announces each test binary, says how many tests it is about to run, and
//! prints a line per test as it finishes. That is a done-out-of-total for unit
//! tests, for each integration test file, and for doc tests — the tiers a
//! project actually has, not ones guessed at.
//!
//! `pytest` gives a total up front and a percentage per line, so it is one
//! tier. Runners that print no total (`go test`, most of `npm test`) give
//! nothing to measure against, and get no bars rather than invented ones.
//!
//! Pure: it reads text. The run view calls it on the step's log each render,
//! which is cheap next to everything else a render does.

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum Kind {
    Unit,
    Integration,
    EndToEnd,
    Doc,
    /// A runner that does not say which kind: everything it ran.
    All,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Unit => "unit",
            Kind::Integration => "integration",
            Kind::EndToEnd => "end-to-end",
            Kind::Doc => "doc",
            Kind::All => "tests",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Tier {
    pub kind: Kind,
    pub total: u32,
    pub done: u32,
    pub failed: u32,
}

impl Tier {
    pub fn complete(&self) -> bool {
        self.done >= self.total
    }
}

/// Every tier that has said how many tests it holds, in a fixed order: unit,
/// integration, end-to-end, doc. Empty when the output gives nothing to
/// measure against.
pub fn parse(log: &str) -> Vec<Tier> {
    let mut tiers: Vec<Tier> = vec![];
    let mut current: Option<Kind> = None;
    let mut pytest = false;

    let tier = |tiers: &mut Vec<Tier>, kind: Kind| -> usize {
        match tiers.iter().position(|t| t.kind == kind) {
            Some(i) => i,
            None => {
                tiers.push(Tier {
                    kind,
                    total: 0,
                    done: 0,
                    failed: 0,
                });
                tiers.len() - 1
            }
        }
    };

    for raw in log.lines() {
        let line = raw.trim();

        // ── cargo test ──
        if let Some(rest) = line.strip_prefix("Running ") {
            current = Some(if rest.starts_with("unittests ") {
                Kind::Unit
            } else {
                kind_of_file(rest.split_whitespace().next().unwrap_or(""))
            });
            continue;
        }
        if line.starts_with("Doc-tests ") {
            current = Some(Kind::Doc);
            continue;
        }
        if let (Some(kind), Some(n)) = (current, count_between(line, "running ", " test")) {
            let i = tier(&mut tiers, kind);
            tiers[i].total += n;
            continue;
        }
        if let Some(kind) = current {
            if line.starts_with("test ") && !line.starts_with("test result:") {
                if let Some((_, verdict)) = line.rsplit_once(" ... ") {
                    let i = tier(&mut tiers, kind);
                    tiers[i].done += 1;
                    if verdict.starts_with("FAILED") {
                        tiers[i].failed += 1;
                    }
                }
                continue;
            }
        }

        // ── pytest ──
        if let Some(n) = count_between(line, "collected ", " item") {
            pytest = true;
            let i = tier(&mut tiers, Kind::All);
            tiers[i].total = n;
            continue;
        }
        if pytest {
            if let Some(pct) = percent_at_end(line) {
                let i = tier(&mut tiers, Kind::All);
                let t = &mut tiers[i];
                t.done = ((pct as f64 / 100.0) * t.total as f64).round() as u32;
                let marks = line.split('[').next().unwrap_or("");
                let marks = marks.split_whitespace().last().unwrap_or("");
                t.failed += marks.chars().filter(|c| *c == 'F' || *c == 'E').count() as u32;
            }
        }
    }

    tiers.retain(|t| t.total > 0);
    for t in &mut tiers {
        t.done = t.done.min(t.total);
        t.failed = t.failed.min(t.done);
    }
    tiers.sort_by_key(|t| t.kind);
    tiers
}

/// An integration test file, unless its name says it is end-to-end.
fn kind_of_file(path: &str) -> Kind {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    if ["e2e", "end_to_end", "end-to-end", "endtoend"]
        .iter()
        .any(|m| name.contains(m))
    {
        Kind::EndToEnd
    } else {
        Kind::Integration
    }
}

/// The number in `"<before>N<after>…"`, as in `running 12 tests` or
/// `collected 40 items`.
fn count_between(line: &str, before: &str, after: &str) -> Option<u32> {
    let rest = line.strip_prefix(before)?;
    let (n, tail) = rest.split_once(' ')?;
    if !format!(" {tail}").starts_with(after) {
        return None;
    }
    n.parse().ok()
}

/// pytest's `[ 40%]` at the end of a progress line.
fn percent_at_end(line: &str) -> Option<u32> {
    let inner = line.strip_suffix("%]")?;
    let (_, n) = inner.rsplit_once('[')?;
    n.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARGO: &str = "\
$ cargo test   (detected from Cargo.toml)
   Compiling demo v0.1.0
    Finished `test` profile
     Running unittests src/lib.rs (target/debug/deps/demo-1a2b)

running 3 tests
test a::one ... ok
test a::two ... FAILED
test a::three ... ignored

test result: FAILED. 1 passed; 1 failed; 1 ignored

     Running tests/api.rs (target/debug/deps/api-3c4d)

running 2 tests
test creates ... ok
";

    fn find(tiers: &[Tier], kind: Kind) -> Tier {
        tiers.iter().find(|t| t.kind == kind).cloned().unwrap()
    }

    #[test]
    fn cargo_counts_each_binary_against_what_it_announced() {
        let tiers = parse(CARGO);
        let unit = find(&tiers, Kind::Unit);
        assert_eq!((unit.done, unit.total, unit.failed), (3, 3, 1));
        let integration = find(&tiers, Kind::Integration);
        assert_eq!((integration.done, integration.total), (1, 2));
        assert!(!integration.complete());
    }

    #[test]
    fn the_result_line_is_not_a_test() {
        let unit = find(&parse(CARGO), Kind::Unit);
        assert_eq!(unit.done, 3);
    }

    #[test]
    fn an_e2e_test_file_is_its_own_tier() {
        let log = "Running tests/e2e_checkout.rs (x)\nrunning 4 tests\ntest buys ... ok\n";
        let e2e = find(&parse(log), Kind::EndToEnd);
        assert_eq!((e2e.done, e2e.total), (1, 4));
    }

    #[test]
    fn several_crates_add_up_in_one_tier() {
        let log = "Running unittests src/lib.rs (a)\nrunning 2 tests\n\
                   Running unittests src/main.rs (b)\nrunning 5 tests\n";
        assert_eq!(find(&parse(log), Kind::Unit).total, 7);
    }

    #[test]
    fn doc_tests_come_last_and_binaries_without_tests_are_left_out() {
        let log = "Running unittests src/main.rs (a)\nrunning 0 tests\n\
                   Running tests/api.rs (b)\nrunning 1 test\ntest x ... ok\n\
                   Doc-tests demo\nrunning 2 tests\ntest src/lib.rs - f (line 3) ... ok\n";
        let kinds: Vec<Kind> = parse(log).iter().map(|t| t.kind).collect();
        assert_eq!(kinds, vec![Kind::Integration, Kind::Doc]);
    }

    #[test]
    fn pytest_is_one_tier_driven_by_its_percentages() {
        let log =
            "collected 20 items\n\ntests/test_a.py ..F..   [ 25%]\ntests/test_b.py ....  [ 45%]\n";
        let all = find(&parse(log), Kind::All);
        assert_eq!((all.done, all.total, all.failed), (9, 20, 1));
    }

    #[test]
    fn a_runner_that_gives_no_total_gets_no_bars() {
        assert!(parse("ok  \texample.com/pkg\t0.2s\nPASS\n").is_empty());
    }
}
