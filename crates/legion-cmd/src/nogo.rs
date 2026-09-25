//! The no-go list (FR-CMD-025) and the command key a confirmation matches on
//! (FR-CMD-026).
//!
//! A no-go entry names a command that never runs. It matches one resolved
//! [`Invocation`] -- a binary and its arguments as the splitter produced them
//! -- never the literal command text, so a flag-order variant (`rm -fr /`,
//! `rm / -rf`) and a wrapper variant (the positions FR-CMD-007 resolves: behind
//! `sudo` or `env`, inside `sh -c`, after a pipe or list operator) hit the same
//! entry. Every entry is data: a binary set and argument predicates, never a
//! per-entry branch in Rust code (FR-CMD-011). The built-in entries live in
//! [`builtin_no_go`]; the policy file's `no_go` array adds entries of the same
//! shape and can never remove or weaken a built-in one.
//!
//! [`command_key`] is the canonical parsed form of a command: two commands
//! that differ only in whitespace or quoting yield the same key, and two whose
//! parsed words or operators differ do not.

use serde::{Deserialize, Serialize};

use crate::splitter::{self, Invocation, Position, ScanError};

/// One no-go entry (FR-CMD-025). It matches on parsed arguments from the
/// splitter's [`crate::Scan`], never on the literal command text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoGoEntry {
    /// Stable id: recorded on the incident row and used as the repeat key
    /// (FR-CMD-027).
    pub id: String,
    /// Binary names that match exactly (`rm`, `chmod`).
    pub binaries: Vec<String>,
    /// Binary name prefixes that match (`mkfs.` for `mkfs.ext4`).
    pub binary_prefixes: Vec<String>,
    /// When set, the invocation must sit at this grammar position (the fork
    /// bomb's self-call sits in a function body).
    pub position: Option<Position>,
    /// All must hold.
    pub predicates: Vec<NoGoPredicate>,
}

/// An argument-level predicate over one invocation's words, each word read in
/// its canonical form ([`canonical_word`]): the value the shell sees, with any
/// quoted special character escaped so a quoted `'~'` is not a home directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoGoPredicate {
    /// Some option word carries this flag: a short flag inside a `-xyz`
    /// cluster, or a long flag as `--name` or `--name=value`. Words after a
    /// bare `--` are operands, not flags.
    Flag { short: Vec<char>, long: Vec<String> },
    /// Some operand (a non-option word) equals one of `equals`, starts with
    /// one of `prefixes`, or ends with one of `suffixes`.
    Operand {
        equals: Vec<String>,
        prefixes: Vec<String>,
        suffixes: Vec<String>,
    },
    /// Some operand is a forced refspec -- it starts with `+` -- whose source
    /// or destination (either side of its first `:`) equals one of `names`.
    /// `+main`, `+HEAD:main` and `+main:refs/heads/main` all force-write the
    /// ref they name, with or without a force flag.
    ForcedRefspec { names: Vec<String> },
}

impl NoGoEntry {
    /// Whether this entry matches `invocation`.
    pub fn matches(&self, invocation: &Invocation) -> bool {
        let binary_matches = self.binaries.contains(&invocation.binary)
            || self
                .binary_prefixes
                .iter()
                .any(|p| invocation.binary.starts_with(p.as_str()));
        if !binary_matches {
            return false;
        }
        if self
            .position
            .is_some_and(|position| position != invocation.position)
        {
            return false;
        }
        let words: Vec<String> = invocation.args.iter().map(|a| canonical_word(a)).collect();
        self.predicates.iter().all(|p| p.holds(&words))
    }
}

impl NoGoPredicate {
    fn holds(&self, words: &[String]) -> bool {
        let (options, operands) = split_options(words);
        match self {
            NoGoPredicate::Flag { short, long } => {
                options.iter().any(|word| match word.strip_prefix("--") {
                    Some(name) => {
                        let name = name.split('=').next().unwrap_or(name);
                        long.iter().any(|l| l == name)
                    }
                    None => word
                        .strip_prefix('-')
                        .is_some_and(|cluster| cluster.chars().any(|c| short.contains(&c))),
                })
            }
            NoGoPredicate::Operand {
                equals,
                prefixes,
                suffixes,
            } => operands.iter().any(|word| {
                equals.iter().any(|e| e == word)
                    || prefixes.iter().any(|p| word.starts_with(p.as_str()))
                    || suffixes.iter().any(|s| word.ends_with(s.as_str()))
            }),
            NoGoPredicate::ForcedRefspec { names } => operands.iter().any(|word| {
                word.strip_prefix('+').is_some_and(|refspec| {
                    let (source, destination) = refspec.split_once(':').unwrap_or((refspec, ""));
                    names.iter().any(|n| n == source || n == destination)
                })
            }),
        }
    }
}

/// Splits canonical words into option words and operands. An option word
/// starts with `-` and is longer than `-` alone; a bare `--` ends the options,
/// and every word after it is an operand.
fn split_options(words: &[String]) -> (Vec<&str>, Vec<&str>) {
    let mut options = Vec::new();
    let mut operands = Vec::new();
    let mut ended = false;
    for word in words {
        if ended {
            operands.push(word.as_str());
        } else if word == "--" {
            ended = true;
        } else if word.len() > 1 && word.starts_with('-') {
            options.push(word.as_str());
        } else {
            operands.push(word.as_str());
        }
    }
    (options, operands)
}

/// The first no-go entry matching any of `invocations`: built-in entries are
/// listed before policy-file entries, so a policy entry reusing a built-in id
/// never takes its place.
pub(crate) fn first_match<'a>(
    entries: &'a [NoGoEntry],
    invocations: &[Invocation],
) -> Option<&'a NoGoEntry> {
    entries
        .iter()
        .find(|entry| invocations.iter().any(|inv| entry.matches(inv)))
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn flag(short: &[char], long: &[&str]) -> NoGoPredicate {
    NoGoPredicate::Flag {
        short: short.to_vec(),
        long: strings(long),
    }
}

fn operand(equals: &[&str], prefixes: &[&str], suffixes: &[&str]) -> NoGoPredicate {
    NoGoPredicate::Operand {
        equals: strings(equals),
        prefixes: strings(prefixes),
        suffixes: strings(suffixes),
    }
}

/// Id of the recursive forced delete of `/`, `/*`, `~` or `$HOME`.
pub const RM_ROOT: &str = "rm-recursive-force-root";
/// Id of `mkfs` on any device.
pub const MKFS: &str = "mkfs-device";
/// Id of `dd` whose output is a disk device.
pub const DD_DISK: &str = "dd-to-disk";
/// Id of the classic fork bomb.
pub const FORK_BOMB: &str = "fork-bomb";
/// Id of `chmod -R` or `chown -R` on `/`.
pub const CHMOD_CHOWN_ROOT: &str = "chmod-chown-recursive-root";
/// Id of a forced `git push` to main or master.
pub const FORCE_PUSH_MAIN: &str = "git-force-push-main";
/// Id of a `git push` whose `+` refspec force-writes main or master.
pub const FORCE_REFSPEC_MAIN: &str = "git-force-refspec-main";

/// The ref names a forced push may not write.
const PROTECTED_BRANCHES: [&str; 4] = ["main", "master", "refs/heads/main", "refs/heads/master"];

/// The disk device paths a raw block copy may not write to: whole disks and
/// partitions, never `/dev/null` or `/dev/zero`.
const DISK_DEVICES: [&str; 8] = [
    "/dev/sd",
    "/dev/hd",
    "/dev/vd",
    "/dev/xvd",
    "/dev/nvme",
    "/dev/mmcblk",
    "/dev/disk",
    "/dev/rdisk",
];

/// The built-in entries, embedded in the binary (FR-CMD-025). Present when
/// the policy file is absent or empty.
pub fn builtin_no_go() -> Vec<NoGoEntry> {
    let dd_outputs: Vec<String> = DISK_DEVICES.iter().map(|d| format!("of={d}")).collect();
    let dd_outputs: Vec<&str> = dd_outputs.iter().map(String::as_str).collect();
    vec![
        NoGoEntry {
            id: RM_ROOT.to_string(),
            binaries: strings(&["rm"]),
            binary_prefixes: Vec::new(),
            position: None,
            predicates: vec![
                flag(&['r', 'R'], &["recursive"]),
                flag(&['f'], &["force"]),
                operand(
                    &[
                        "/", "/*", "~", "~/", "$HOME", "$HOME/", "${HOME}", "${HOME}/",
                    ],
                    &[],
                    &[],
                ),
            ],
        },
        NoGoEntry {
            id: MKFS.to_string(),
            binaries: strings(&["mkfs"]),
            binary_prefixes: strings(&["mkfs."]),
            position: None,
            predicates: vec![operand(&[], &["/dev/"], &[])],
        },
        NoGoEntry {
            id: DD_DISK.to_string(),
            binaries: strings(&["dd"]),
            binary_prefixes: Vec::new(),
            position: None,
            predicates: vec![operand(&[], &dd_outputs, &[])],
        },
        NoGoEntry {
            // `:(){ :|:& };:` defines a function named `:` whose body runs
            // `:` twice; the self-call inside the function body is the match.
            id: FORK_BOMB.to_string(),
            binaries: strings(&[":"]),
            binary_prefixes: Vec::new(),
            position: Some(Position::FunctionBody),
            predicates: Vec::new(),
        },
        NoGoEntry {
            id: CHMOD_CHOWN_ROOT.to_string(),
            binaries: strings(&["chmod", "chown"]),
            binary_prefixes: Vec::new(),
            position: None,
            predicates: vec![flag(&['R'], &["recursive"]), operand(&["/"], &[], &[])],
        },
        NoGoEntry {
            id: FORCE_PUSH_MAIN.to_string(),
            binaries: strings(&["git"]),
            binary_prefixes: Vec::new(),
            position: None,
            predicates: vec![
                operand(&["push"], &[], &[]),
                flag(&['f'], &["force", "force-with-lease"]),
                operand(
                    &PROTECTED_BRANCHES,
                    &[],
                    &[":main", ":master", ":refs/heads/main", ":refs/heads/master"],
                ),
            ],
        },
        NoGoEntry {
            id: FORCE_REFSPEC_MAIN.to_string(),
            binaries: strings(&["git"]),
            binary_prefixes: Vec::new(),
            position: None,
            predicates: vec![
                operand(&["push"], &[], &[]),
                NoGoPredicate::ForcedRefspec {
                    names: strings(&PROTECTED_BRANCHES),
                },
            ],
        },
    ]
}

/// The harness `permissions.deny` text patterns that mirror each built-in
/// entry (FR-CMD-025): at least one per common written form. This layer is a
/// backstop and is not complete; route's argument match is the check.
///
/// The harness reads `*` in a pattern as a wildcard, so the `rm -rf /*` form
/// has no pattern: `Bash(rm -rf /*)` would also deny every recursive delete
/// of an absolute path. route still refuses that form.
pub const BUILTIN_DENY_PATTERNS: &[(&str, &[&str])] = &[
    (
        RM_ROOT,
        &[
            "Bash(rm -rf /)",
            "Bash(rm -fr /)",
            "Bash(rm -r -f /)",
            "Bash(rm -rf ~)",
            "Bash(rm -rf ~/)",
            "Bash(rm -rf $HOME)",
            "Bash(rm -rf \"$HOME\")",
            "Bash(sudo rm -rf /)",
        ],
    ),
    (
        MKFS,
        &[
            "Bash(mkfs *)",
            "Bash(mkfs.*)",
            "Bash(sudo mkfs *)",
            "Bash(sudo mkfs.*)",
        ],
    ),
    (
        DD_DISK,
        &[
            "Bash(dd * of=/dev/sd*)",
            "Bash(dd * of=/dev/disk*)",
            "Bash(dd * of=/dev/rdisk*)",
            "Bash(dd * of=/dev/nvme*)",
            "Bash(sudo dd * of=/dev/*)",
        ],
    ),
    (FORK_BOMB, &["Bash(:(){ :|:& };:)"]),
    (
        CHMOD_CHOWN_ROOT,
        &[
            "Bash(chmod -R * /)",
            "Bash(chown -R * /)",
            "Bash(sudo chmod -R * /)",
            "Bash(sudo chown -R * /)",
        ],
    ),
    (
        FORCE_PUSH_MAIN,
        &[
            "Bash(git push --force * main)",
            "Bash(git push --force * master)",
            "Bash(git push -f * main)",
            "Bash(git push -f * master)",
            "Bash(git push --force-with-lease * main)",
            "Bash(git push --force-with-lease * master)",
        ],
    ),
    (
        FORCE_REFSPEC_MAIN,
        &[
            "Bash(git push * +main)",
            "Bash(git push * +master)",
            "Bash(git push * +HEAD:main)",
            "Bash(git push * +HEAD:master)",
        ],
    ),
];

/// The canonical parsed form of a command (FR-CMD-026): the words and
/// operators of the command, each word in its canonical form. Two commands
/// that differ only in whitespace or quoting yield the same key; a different
/// word, operator, or redirect target yields a different one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CommandKey(String);

impl CommandKey {
    /// The key as stored text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Rebuilds a key from text [`CommandKey::as_str`] produced.
    pub fn from_stored(text: impl Into<String>) -> Self {
        Self(text.into())
    }
}

/// One token of a [`CommandKey`].
#[derive(Serialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
enum KeyToken {
    Word(String),
    Operator(String),
}

/// The canonical parsed form of a command. Pure.
///
/// The command must parse ([`splitter::scan`]): a command route cannot parse
/// has no key, so it cannot be confirmed. The key is built from the shell
/// tokenizer's stream -- every word (in canonical form) and every operator,
/// including redirect operators and their targets, which the splitter's
/// invocations do not carry -- so no part of the command falls outside it.
pub fn command_key(command: &str) -> Result<CommandKey, ScanError> {
    splitter::scan(command)?;
    let tokens = brush_parser::tokenize_str(command).map_err(|e| ScanError::Tokenize {
        offset: command.len(),
        detail: e.to_string(),
    })?;
    let key: Vec<KeyToken> = tokens
        .into_iter()
        .map(|token| match token {
            brush_parser::Token::Word(text, _) => KeyToken::Word(canonical_word(&text)),
            brush_parser::Token::Operator(text, _) => KeyToken::Operator(text),
        })
        .collect();
    let text = serde_json::to_string(&key).map_err(|e| ScanError::Tokenize {
        offset: command.len(),
        detail: e.to_string(),
    })?;
    Ok(CommandKey(text))
}

/// Characters that carry shell meaning when unquoted. A literal (quoted or
/// escaped) occurrence is written back escaped, so a quoted `'*'` and an
/// unquoted `*` stay distinct while `'a'` and `a` become the same word.
fn is_special(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '\\' | '\''
                | '"'
                | '$'
                | '`'
                | '*'
                | '?'
                | '['
                | ']'
                | '~'
                | '{'
                | '}'
                | '|'
                | '&'
                | ';'
                | '<'
                | '>'
                | '('
                | ')'
                | '#'
                | '!'
        )
}

fn push_literal(out: &mut String, c: char) {
    if is_special(c) {
        out.push('\\');
    }
    out.push(c);
}

/// One shell word in canonical form: the quoting removed, and every character
/// that is literal only because it was quoted or escaped written back with a
/// backslash. `'/'`, `"/"` and `/` are all `/`; `"$HOME"` and `$HOME` are
/// `$HOME` (the expansion survives double quotes); `'$HOME'` is `\$HOME`, a
/// literal name, not the home directory.
pub(crate) fn canonical_word(raw: &str) -> String {
    #[derive(PartialEq)]
    enum Mode {
        Bare,
        Single,
        Double,
    }
    let mut out = String::with_capacity(raw.len());
    let mut mode = Mode::Bare;
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match mode {
            Mode::Bare => match c {
                '\\' => {
                    if let Some(next) = chars.next() {
                        push_literal(&mut out, next);
                    }
                }
                '\'' => mode = Mode::Single,
                '"' => mode = Mode::Double,
                other => out.push(other),
            },
            Mode::Single => match c {
                '\'' => mode = Mode::Bare,
                other => push_literal(&mut out, other),
            },
            Mode::Double => match c {
                '"' => mode = Mode::Bare,
                '\\' => match chars.peek().copied() {
                    Some(next @ ('$' | '`' | '"' | '\\')) => {
                        chars.next();
                        push_literal(&mut out, next);
                    }
                    Some('\n') => {
                        chars.next();
                    }
                    _ => push_literal(&mut out, '\\'),
                },
                // Expansions keep their meaning inside double quotes.
                '$' | '`' | '{' | '}' | '(' | ')' => out.push(c),
                other => push_literal(&mut out, other),
            },
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invocations(command: &str) -> Vec<Invocation> {
        splitter::scan(command).expect("parses").invocations
    }

    fn matched(command: &str) -> Option<String> {
        let entries = builtin_no_go();
        first_match(&entries, &invocations(command)).map(|e| e.id.clone())
    }

    #[test]
    fn rm_recursive_force_on_root_home_and_glob_matches_in_any_flag_order() {
        for command in [
            "rm -rf /",
            "rm -fr /",
            "rm -r -f /",
            "rm / -rf",
            "rm -rf /*",
            "rm -rf ~",
            "rm -rf $HOME",
            "rm -rf \"$HOME\"",
            "rm -rf ${HOME}",
            "rm -Rf /",
            "rm --recursive --force /",
            "rm -rf -- /",
            "rm -rf '/'",
        ] {
            assert_eq!(matched(command).as_deref(), Some(RM_ROOT), "{command}");
        }
    }

    #[test]
    fn rm_without_both_flags_or_on_another_path_does_not_match() {
        for command in [
            "rm -r /tmp/x",
            "rm -rf /tmp/x",
            "rm -r /",
            "rm -f /",
            "rm -rf '~'",
            "rm -rf '$HOME'",
            "rm -rf \"/*\"",
        ] {
            assert_eq!(matched(command), None, "{command}");
        }
    }

    #[test]
    fn mkfs_on_any_device_matches() {
        for command in ["mkfs.ext4 /dev/sda1", "mkfs -t ext4 /dev/nvme0n1p1"] {
            assert_eq!(matched(command).as_deref(), Some(MKFS), "{command}");
        }
        assert_eq!(matched("mkfs.ext4 disk.img"), None);
    }

    #[test]
    fn dd_to_a_disk_matches_and_dd_to_null_does_not() {
        assert_eq!(
            matched("dd if=/dev/zero of=/dev/sda").as_deref(),
            Some(DD_DISK)
        );
        assert_eq!(
            matched("dd if=x.img of=/dev/disk2 bs=1m").as_deref(),
            Some(DD_DISK)
        );
        assert_eq!(matched("dd if=/dev/sda of=/dev/null"), None);
        assert_eq!(matched("dd if=/dev/zero of=out.img"), None);
    }

    #[test]
    fn the_fork_bomb_matches() {
        assert_eq!(matched(":(){ :|:& };:").as_deref(), Some(FORK_BOMB));
    }

    #[test]
    fn chmod_and_chown_recursive_on_root_match() {
        for command in ["chmod -R 777 /", "chown -R nobody /", "chmod 777 -R /"] {
            assert_eq!(
                matched(command).as_deref(),
                Some(CHMOD_CHOWN_ROOT),
                "{command}"
            );
        }
        assert_eq!(matched("chmod -R 755 /tmp/x"), None);
        assert_eq!(matched("chmod 755 /"), None);
    }

    #[test]
    fn a_forced_push_to_main_or_master_matches_and_to_another_branch_does_not() {
        for command in [
            "git push --force origin main",
            "git push -f origin master",
            "git push --force-with-lease origin main",
            "git push origin main --force",
            "git push --force-with-lease=main:abc origin main",
            "git push -f origin HEAD:main",
        ] {
            assert_eq!(
                matched(command).as_deref(),
                Some(FORCE_PUSH_MAIN),
                "{command}"
            );
        }
        for command in [
            "git push --force origin feature",
            "git push -f origin fix/main-menu",
            "git push origin main",
        ] {
            assert_eq!(matched(command), None, "{command}");
        }
    }

    #[test]
    fn a_plus_refspec_that_force_writes_main_or_master_matches() {
        for command in [
            "git push origin +main",
            "git push -f origin +main",
            "git push origin +master",
            "git push origin +HEAD:main",
            "git push origin +feature:refs/heads/master",
            "git push origin +main:backup",
            "git push origin feature +refs/heads/main",
        ] {
            let hit = matched(command);
            assert!(
                hit.as_deref() == Some(FORCE_REFSPEC_MAIN)
                    || hit.as_deref() == Some(FORCE_PUSH_MAIN),
                "{command} -> {hit:?}"
            );
        }
        assert_eq!(
            matched("git push origin +main").as_deref(),
            Some(FORCE_REFSPEC_MAIN)
        );
        for command in [
            "git push origin +feature",
            "git push origin +HEAD:feature",
            "git push origin main",
            "git push origin HEAD:main",
            "git fetch origin +main:main",
        ] {
            assert_eq!(matched(command), None, "{command}");
        }
    }

    #[test]
    fn every_builtin_entry_has_a_permissions_deny_pattern() {
        for entry in builtin_no_go() {
            let patterns = BUILTIN_DENY_PATTERNS
                .iter()
                .find(|(id, _)| *id == entry.id)
                .map(|(_, patterns)| *patterns)
                .unwrap_or(&[]);
            assert!(!patterns.is_empty(), "{} has no deny pattern", entry.id);
            assert!(patterns.iter().all(|p| p.starts_with("Bash(")));
        }
    }

    #[test]
    fn builtin_ids_are_unique() {
        let entries = builtin_no_go();
        for (i, entry) in entries.iter().enumerate() {
            assert!(entries[i + 1..].iter().all(|other| other.id != entry.id));
        }
    }

    #[test]
    fn whitespace_and_quoting_differences_yield_the_same_key() {
        let base = command_key("git push origin main").expect("key");
        for same in [
            "git  push   origin main",
            "git 'push' origin \"main\"",
            "git push origin ma\\in",
            "  git push origin main  ",
        ] {
            assert_eq!(command_key(same).expect("key"), base, "{same}");
        }
    }

    #[test]
    fn different_words_operators_or_redirects_yield_different_keys() {
        let base = command_key("curl example.com | cat").expect("key");
        for different in [
            "curl example.org | cat",
            "curl example.com ; cat",
            "curl example.com | cat > out",
            "curl example.com",
        ] {
            assert_ne!(command_key(different).expect("key"), base, "{different}");
        }
        assert_ne!(
            command_key("echo 'a b'").expect("key"),
            command_key("echo a b").expect("key")
        );
        assert_ne!(
            command_key("echo '$HOME'").expect("key"),
            command_key("echo $HOME").expect("key")
        );
    }

    #[test]
    fn a_command_that_does_not_parse_has_no_key() {
        assert!(command_key("echo 'unterminated").is_err());
    }

    #[test]
    fn canonical_word_removes_quoting_and_escapes_quoted_specials() {
        assert_eq!(canonical_word("'/'"), "/");
        assert_eq!(canonical_word("\"$HOME\""), "$HOME");
        assert_eq!(canonical_word("'$HOME'"), "\\$HOME");
        assert_eq!(canonical_word("\"~\""), "\\~");
        assert_eq!(canonical_word("'a b'"), "a\\ b");
        assert_eq!(canonical_word("a\\ b"), "a\\ b");
        assert_eq!(canonical_word("\"${HOME}\""), "${HOME}");
    }
}
