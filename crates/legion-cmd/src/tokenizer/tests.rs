use super::*;

fn invocation<'a>(scan: &'a Scan, binary: &str) -> Option<&'a Invocation> {
    scan.invocations.iter().find(|inv| inv.binary == binary)
}

// -- Position's Display and its independently-derived serde name agree ---

#[test]
fn position_display_matches_the_serde_kebab_case_name_for_every_variant() {
    for position in Position::ALL {
        let json = format!("\"{position}\"");
        let parsed: Position = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("Display name `{position}` did not deserialize: {e}"));
        assert_eq!(
            parsed, position,
            "Display and serde disagree for {position:?}"
        );
    }
}

// -- Plain, first-position visibility --------------------------------

#[test]
fn plain_command_resolves_at_first_position() {
    let scan = scan("grep -rn foo src").expect("valid command");
    let inv = invocation(&scan, "grep").expect("grep resolved");
    assert_eq!(inv.position, Position::First);
    assert_eq!(inv.depth, 0);
    assert_eq!(inv.args, vec!["-rn", "foo", "src"]);
}

// -- NFR-CMD-002: each visibility path has a unit test ---------------

#[test]
fn managed_binary_after_pipe_is_routed_identically() {
    let first = scan("grep -rn foo src").expect("valid");
    let piped = scan("cat src/a.rs | grep -rn foo src").expect("valid");
    let a = invocation(&first, "grep").expect("first");
    let b = invocation(&piped, "grep").expect("after pipe");
    assert_eq!(a.binary, b.binary);
    assert_eq!(a.args, b.args);
    assert_eq!(a.position, Position::First);
    assert_eq!(b.position, Position::AfterOperator);
}

#[test]
fn managed_binary_behind_environment_prefix_is_routed_identically() {
    let first = scan("grep -rn foo src").expect("valid");
    let prefixed = scan("FOO=1 grep -rn foo src").expect("valid");
    let a = invocation(&first, "grep").expect("first");
    let b = invocation(&prefixed, "grep").expect("behind assignment");
    assert_eq!(a.binary, b.binary);
    assert_eq!(a.args, b.args);
    assert_eq!(b.position, Position::AfterAssignment);
}

#[test]
fn managed_binary_inside_inline_wrapper_is_routed_identically() {
    let first = scan("grep -rn foo src").expect("valid");
    let wrapped = scan("env grep -rn foo src").expect("valid");
    let a = invocation(&first, "grep").expect("first");
    let b = invocation(&wrapped, "grep").expect("inside wrapper");
    assert_eq!(a.binary, b.binary);
    assert_eq!(a.args, b.args);
    assert_eq!(b.position, Position::Wrapper);
}

// -- Wrapper table (FR-CMD-007) ---------------------------------------

#[test]
fn sudo_wrapper_reaches_the_wrapped_command() {
    let scan = scan("sudo -u root find / -name core").expect("valid");
    assert!(invocation(&scan, "sudo").is_some());
    let find = invocation(&scan, "find").expect("find resolved through sudo");
    assert_eq!(find.position, Position::Wrapper);
}

#[test]
fn timeout_skips_its_duration_positional() {
    let scan = scan("timeout 30 gh run watch 123456").expect("valid");
    let gh = invocation(&scan, "gh").expect("gh resolved through timeout");
    assert_eq!(gh.position, Position::Wrapper);
    assert_eq!(gh.args, vec!["run", "watch", "123456"]);
}

#[test]
fn command_v_is_a_lookup_not_an_invocation_of_its_target() {
    let scan = scan("command -v gh").expect("valid");
    assert!(invocation(&scan, "command").is_some());
    assert!(invocation(&scan, "gh").is_none());
}

#[test]
fn docker_exec_reaches_the_wrapped_command() {
    let scan = scan("docker exec build-container grep -c ERROR /var/log/app.log").expect("valid");
    assert!(invocation(&scan, "docker").is_some());
    let grep = invocation(&scan, "grep").expect("grep resolved through docker exec");
    assert_eq!(grep.position, Position::Wrapper);
}

#[test]
fn env_dash_s_splits_its_payload_and_resolves_the_head() {
    let scan = scan("env -S \"grep -rn foo\" src").expect("valid");
    let grep = invocation(&scan, "grep").expect("grep resolved through env -S");
    assert_eq!(grep.position, Position::Wrapper);
    assert_eq!(grep.args, vec!["-rn", "foo"]);
}

#[test]
fn env_dash_s_belonging_to_the_wrapped_command_is_not_envs_own() {
    // `-S` on `sort` is `sort`'s own flag (a 1 GiB buffer size), not env's
    // string-splitting `-S`. Scanning `rest` for the first `-S` anywhere in
    // it (rather than walking env's own leading options one at a time)
    // would misread this as env's `-S` and turn "1G" into a bogus head
    // invocation while losing `sort` entirely.
    let scan = scan("env sort -S 1G file").expect("valid");
    let sort = invocation(&scan, "sort").expect("sort resolved as env's wrapped command");
    assert_eq!(sort.position, Position::Wrapper);
    assert_eq!(sort.args, vec!["-S", "1G", "file"]);
    assert!(invocation(&scan, "1G").is_none());
}

#[test]
fn docker_global_options_precede_exec() {
    let scan = scan("docker -H tcp://h exec box grep -c x f").expect("valid");
    assert!(invocation(&scan, "docker").is_some());
    let grep = invocation(&scan, "grep").expect("grep resolved through docker exec");
    assert_eq!(grep.position, Position::Wrapper);
}

#[test]
fn empty_basename_is_a_dynamic_command_not_a_silent_return() {
    let scan = scan("dir/ -rn foo src").expect("valid");
    assert!(
        scan.opaque
            .iter()
            .any(|o| matches!(o, Opaque::DynamicCommand))
    );
    assert!(scan.invocations.is_empty());
}

#[test]
fn script_extension_match_is_ascii_case_insensitive() {
    let scan = scan("./scripts/RELEASE.SH --dry-run").expect("valid");
    assert!(scan.opaque.iter().any(|o| matches!(o, Opaque::ScriptFile)));
    assert!(scan.invocations.is_empty());
}

#[test]
fn npx_is_a_single_word_wrapper() {
    let scan = scan("npx -y rg --version").expect("valid");
    assert!(invocation(&scan, "npx").is_some());
    let rg = invocation(&scan, "rg").expect("rg resolved through npx");
    assert_eq!(rg.position, Position::Wrapper);
}

#[test]
fn pnpx_is_a_single_word_wrapper() {
    let scan = scan("pnpx rg --version").expect("valid");
    let rg = invocation(&scan, "rg").expect("rg resolved through pnpx");
    assert_eq!(rg.position, Position::Wrapper);
}

#[test]
fn bunx_is_a_single_word_wrapper() {
    let scan = scan("bunx rg --version").expect("valid");
    let rg = invocation(&scan, "rg").expect("rg resolved through bunx");
    assert_eq!(rg.position, Position::Wrapper);
}

#[test]
fn pnpm_exec_is_a_two_word_wrapper() {
    let scan = scan("pnpm exec wrangler tail").expect("valid");
    assert!(invocation(&scan, "pnpm").is_some());
    let wrangler = invocation(&scan, "wrangler").expect("wrangler resolved through pnpm exec");
    assert_eq!(wrangler.position, Position::Wrapper);
}

#[test]
fn pnpm_dlx_is_a_two_word_wrapper() {
    let scan = scan("pnpm dlx create-react-app my-app").expect("valid");
    let created = invocation(&scan, "create-react-app").expect("resolved through pnpm dlx");
    assert_eq!(created.position, Position::Wrapper);
}

#[test]
fn yarn_dlx_is_a_two_word_wrapper() {
    let scan = scan("yarn dlx cowsay hello").expect("valid");
    let cowsay = invocation(&scan, "cowsay").expect("cowsay resolved through yarn dlx");
    assert_eq!(cowsay.position, Position::Wrapper);
}

// -- A value-taking global option before exec/dlx must not lose the
// -- wrapped command (round-2 review, reproduced): before per-tool global
// -- option tables existed, `--filter foo` (etc.) was treated as a boolean
// -- flag, so its value word "foo" landed where "exec" was expected,
// -- silently failing to match and leaving only the runner resolved.

#[test]
fn pnpm_filter_precedes_exec() {
    let scan = scan("pnpm --filter foo exec eslint .").expect("valid");
    let eslint = invocation(&scan, "eslint").expect("eslint resolved through pnpm --filter exec");
    assert_eq!(eslint.position, Position::Wrapper);
}

#[test]
fn pnpm_dash_c_dir_precedes_exec() {
    let scan = scan("pnpm -C packages/foo exec eslint .").expect("valid");
    let eslint = invocation(&scan, "eslint").expect("eslint resolved through pnpm -C exec");
    assert_eq!(eslint.position, Position::Wrapper);
}

#[test]
fn npm_prefix_precedes_exec() {
    let scan = scan("npm --prefix dir exec eslint .").expect("valid");
    let eslint = invocation(&scan, "eslint").expect("eslint resolved through npm --prefix exec");
    assert_eq!(eslint.position, Position::Wrapper);
}

#[test]
fn npm_workspace_precedes_exec() {
    let scan = scan("npm -w foo exec eslint .").expect("valid");
    let eslint = invocation(&scan, "eslint").expect("eslint resolved through npm -w exec");
    assert_eq!(eslint.position, Position::Wrapper);
}

#[test]
fn yarn_cwd_precedes_exec() {
    let scan = scan("yarn --cwd dir exec eslint .").expect("valid");
    let eslint = invocation(&scan, "eslint").expect("eslint resolved through yarn --cwd exec");
    assert_eq!(eslint.position, Position::Wrapper);
}

#[test]
fn unmodeled_global_option_before_exec_fails_closed_to_unknown_wrapper() {
    // An option this table does not know shifts the position so "exec"
    // is not found where expected, but "exec" still appears later in the
    // command: this is a recognized two-word wrapper we could not
    // correctly unwrap, so it must be opaque, not a silent ordinary
    // invocation of pnpm.
    let scan = scan("pnpm --some-unknown-opt val exec eslint .").expect("valid");
    assert!(
        scan.opaque
            .iter()
            .any(|o| matches!(o, Opaque::UnknownWrapper))
    );
    assert!(invocation(&scan, "eslint").is_none());
}

#[test]
fn bare_pnpm_grep_stays_ordinary_after_the_fail_closed_change() {
    // The mandated benign false positive: no `exec`/`dlx` anywhere in the
    // command, so this must stay a plain invocation, not UnknownWrapper.
    let scan = scan("pnpm grep").expect("valid");
    assert!(
        !scan
            .opaque
            .iter()
            .any(|o| matches!(o, Opaque::UnknownWrapper))
    );
    let pnpm = invocation(&scan, "pnpm").expect("pnpm resolved");
    assert_eq!(pnpm.args, vec!["grep"]);
}

#[test]
fn bare_pnpm_wrangler_stays_ordinary_after_the_fail_closed_change() {
    let scan = scan("pnpm wrangler deploy --env prod").expect("valid");
    assert!(
        !scan
            .opaque
            .iter()
            .any(|o| matches!(o, Opaque::UnknownWrapper))
    );
    assert!(invocation(&scan, "wrangler").is_none());
}

#[test]
fn bare_pnpm_with_a_non_runner_argument_is_an_ordinary_invocation() {
    // FR-CMD-007 revision 6: only `pnpm exec`/`pnpm dlx` are wrappers. A
    // bare `pnpm grep` (the mandated benign false positive) must stay an
    // ordinary invocation of pnpm, never reaching into "grep".
    let scan = scan("pnpm grep").expect("valid");
    let pnpm = invocation(&scan, "pnpm").expect("pnpm resolved");
    assert_eq!(pnpm.args, vec!["grep"]);
    assert!(invocation(&scan, "grep").is_none());
}

#[test]
fn time_is_a_wrapper_not_a_skipped_keyword() {
    let scan = scan("time rg -n foo").expect("valid");
    assert!(invocation(&scan, "time").is_some());
    let rg = invocation(&scan, "rg").expect("rg resolved through time");
    assert_eq!(rg.position, Position::Wrapper);
}

#[test]
fn time_before_a_keyword_still_reaches_the_wrapped_command() {
    let scan = scan("time if grep -q foo bar; then echo hit; fi").expect("valid");
    let grep = invocation(&scan, "grep").expect("grep resolved through time and if");
    assert_eq!(grep.position, Position::Wrapper);
}

// -- Shells: -c, heredoc, script file, stdin -------------------------

#[test]
fn sh_c_inline_script_resolves_at_depth_one() {
    let scan = scan("sh -c 'grep -rn foo src'").expect("valid");
    let grep = invocation(&scan, "grep").expect("grep resolved inside sh -c");
    assert_eq!(grep.position, Position::InlineShell);
    assert_eq!(grep.depth, 1);
}

#[test]
fn bash_heredoc_resolves_the_body() {
    let scan = scan("bash <<'EOF'\ncd src\nrg -n 'fn main' .\nEOF").expect("valid");
    let rg = invocation(&scan, "rg").expect("rg resolved inside the heredoc body");
    assert_eq!(rg.position, Position::HeredocShell);
    assert_eq!(rg.depth, 1);
}

#[test]
fn bash_script_file_is_opaque() {
    let scan = scan("bash run2.sh").expect("valid");
    assert!(scan.opaque.iter().any(|o| matches!(o, Opaque::ScriptFile)));
}

#[test]
fn dot_slash_script_is_opaque() {
    let scan = scan("./scripts/release.sh --dry-run").expect("valid");
    assert!(scan.opaque.iter().any(|o| matches!(o, Opaque::ScriptFile)));
}

// -- Interpreters ------------------------------------------------------

#[test]
fn python_dash_c_body_is_opaque_but_carries_the_body_text() {
    let scan = scan("python3 -c \"import os; print(1)\"").expect("valid");
    let found = scan.opaque.iter().find_map(|o| match o {
        Opaque::Interpreter { interpreter, body } => Some((interpreter.clone(), body.clone())),
        _ => None,
    });
    let (interpreter, body) = found.expect("interpreter body recorded");
    assert_eq!(interpreter, "python3");
    assert!(body.contains("import os"));
}

#[test]
fn awk_program_body_is_an_opaque_interpreter() {
    let scan = scan("awk 'BEGIN{system(\"grep -rn struct src\")}' access.log").expect("valid");
    let found = scan
        .opaque
        .iter()
        .any(|o| matches!(o, Opaque::Interpreter { interpreter, .. } if interpreter == "awk"));
    assert!(found);
}

// -- find -exec ----------------------------------------------------------

#[test]
fn find_exec_resolves_the_executed_command() {
    let scan = scan("find src -name '*.rs' -exec grep -l 'fn route' {} \\;").expect("valid");
    assert!(invocation(&scan, "find").is_some());
    let grep = invocation(&scan, "grep").expect("grep resolved through find -exec");
    assert_eq!(grep.position, Position::FindExec);
    assert_eq!(grep.depth, 1);
}

#[test]
fn find_exec_sh_c_resolves_at_depth_two() {
    let scan =
        scan("find . -name '*.md' -exec sh -c 'grep -l TODO \"$1\"' _ {} \\;").expect("valid");
    let grep = invocation(&scan, "grep").expect("grep resolved through find -exec and sh -c");
    assert_eq!(grep.depth, 2);
}

// -- Substitutions -------------------------------------------------------

#[test]
fn command_substitution_resolves_at_depth_one() {
    let scan = scan("echo \"branch is $(git branch --show-current)\"").expect("valid");
    let git = invocation(&scan, "git").expect("git resolved inside the substitution");
    assert_eq!(git.position, Position::Substitution);
    assert_eq!(git.depth, 1);
}

#[test]
fn heredoc_body_inside_command_substitution_is_literal_text() {
    // The commit-message idiom (RESEARCH-CMD-bounded-tokenizer caveat):
    // an apostrophe inside a heredoc body nested in `$()` must not be
    // read as opening a quote.
    let cmd = "git add src/a.rs && git commit -m \"$(cat <<'EOF'\nfix: don't trust the agent's \"quotes\" (it's a body)\nEOF\n)\" && git push origin HEAD";
    let scan = scan(cmd).expect("the heredoc-in-substitution idiom parses");
    let gits: Vec<&Invocation> = scan
        .invocations
        .iter()
        .filter(|i| i.binary == "git")
        .collect();
    assert!(
        gits.iter()
            .any(|g| g.args.first().map(String::as_str) == Some("commit"))
    );
    assert!(
        gits.iter()
            .any(|g| g.args.first().map(String::as_str) == Some("push"))
    );
}

#[test]
fn too_deep_beyond_max_depth_is_recorded_opaque() {
    let scan = scan("echo $(echo $(echo $(grep -c x f)))").expect("valid");
    assert!(scan.opaque.iter().any(|o| matches!(o, Opaque::TooDeep)));
    assert!(invocation(&scan, "grep").is_none());
}

// -- Function bodies -----------------------------------------------------

#[test]
fn function_keyword_body_is_resolved_as_function_body() {
    let scan = scan("function g { grep -rn \"$@\" src; }; g foo src").expect("valid");
    let grep = invocation(&scan, "grep").expect("grep resolved inside the function body");
    assert_eq!(grep.position, Position::FunctionBody);
}

#[test]
fn name_parens_form_is_a_definition_not_a_call() {
    let scan = scan("search() { rg -n \"$@\" src; }").expect("valid");
    // The definition itself is not a call to a binary named "search".
    assert!(invocation(&scan, "search").is_none());
    let rg = invocation(&scan, "rg").expect("rg resolved inside the function body");
    assert_eq!(rg.position, Position::FunctionBody);
}

// -- Opaque classes named in FR-CMD-007 -----------------------------------

#[test]
fn eval_is_opaque() {
    let scan = scan("eval \"$(ssh-agent -s)\"").expect("valid");
    assert!(invocation(&scan, "eval").is_some());
    assert!(scan.opaque.iter().any(|o| matches!(o, Opaque::Eval)));
}

#[test]
fn dynamic_command_from_a_bare_variable_is_opaque() {
    let scan = scan("$SEARCH -rn foo src").expect("valid");
    assert!(
        scan.opaque
            .iter()
            .any(|o| matches!(o, Opaque::DynamicCommand))
    );
}

#[test]
fn dynamic_command_from_a_whole_word_substitution_is_opaque() {
    let scan = scan("$(which rg) -n foo").expect("valid");
    assert!(
        scan.opaque
            .iter()
            .any(|o| matches!(o, Opaque::DynamicCommand))
    );
}

#[test]
fn dynamic_command_from_positional_args_array_is_opaque() {
    let scan = scan("set -- grep -rn foo src; \"$@\"").expect("valid");
    assert!(
        scan.opaque
            .iter()
            .any(|o| matches!(o, Opaque::DynamicCommand))
    );
}

#[test]
fn dynamic_command_from_an_embedded_unquoted_parameter_expansion_is_opaque() {
    // The shell expands `${IFS}` (commonly whitespace) and then
    // word-splits the result, turning one token into the governed
    // `gh issue list` -- resolvable only by executing it, so it must
    // never be silently allowed as an unrecognized literal binary.
    let scan = scan("gh${IFS}issue${IFS}list").expect("valid");
    assert!(
        scan.invocations.is_empty(),
        "must not resolve an invocation"
    );
    assert!(
        scan.opaque
            .iter()
            .any(|o| matches!(o, Opaque::DynamicCommand))
    );
}

#[test]
fn a_quoted_dollar_inside_an_argument_stays_an_ordinary_argument() {
    // Only the command-name word is checked for an unquoted expansion;
    // a quoted `$` in an ARGUMENT is inert text and must resolve as a
    // plain argument, not trigger anything.
    let scan = scan("grep '$PATTERN' file.txt").expect("valid");
    let inv = invocation(&scan, "grep").expect("grep resolved");
    assert_eq!(inv.args, vec!["$PATTERN", "file.txt"]);
    assert!(scan.opaque.is_empty());
}

#[test]
fn sourced_substitution_is_opaque() {
    let scan = scan("source <(echo \"grep -rn foo src\")").expect("valid");
    assert!(scan.opaque.iter().any(|o| matches!(o, Opaque::Sourced)));
}

#[test]
fn git_shell_alias_is_opaque() {
    let scan = scan("git -c alias.s='!grep -rn foo src' s").expect("valid");
    assert!(scan.opaque.iter().any(|o| matches!(o, Opaque::Alias)));
}

#[test]
fn unknown_wrapper_is_recorded_opaque_not_silently_resolved() {
    let scan = scan("flock /tmp/build.lock grep -rn foo src").expect("valid");
    assert!(invocation(&scan, "flock").is_some());
    assert!(
        scan.opaque
            .iter()
            .any(|o| matches!(o, Opaque::UnknownWrapper))
    );
    assert!(invocation(&scan, "grep").is_none());
}

// -- Benign: things that look like a match but are not --------------------

#[test]
fn git_log_grep_reflog_flag_is_an_ordinary_argument() {
    let scan = scan("git log --grep-reflog=x").expect("valid");
    let git = invocation(&scan, "git").expect("git resolved");
    assert_eq!(git.args, vec!["log", "--grep-reflog=x"]);
    // The scanner never pattern-matches flag substrings: "--grep-reflog"
    // superficially contains "grep" but must never become its own
    // invocation.
    assert!(invocation(&scan, "grep").is_none());
}

#[test]
fn dash_dash_marks_a_literal_pathspec_not_a_flag() {
    let scan = scan("git log -- -Sfoo").expect("valid");
    let git = invocation(&scan, "git").expect("git resolved");
    assert_eq!(git.args, vec!["log", "--", "-Sfoo"]);
}

#[test]
fn pnpm_grep_is_an_ordinary_invocation_of_pnpm() {
    let scan = scan("pnpm grep").expect("valid");
    let pnpm = invocation(&scan, "pnpm").expect("pnpm resolved");
    assert_eq!(pnpm.args, vec!["grep"]);
    // pnpm is not a wrapper here (only `pnpm exec`/`pnpm dlx` are): "grep"
    // is just pnpm's argument, never resolved as its own invocation.
    assert!(invocation(&scan, "grep").is_none());
}

#[test]
fn gh_as_a_for_loop_word_is_never_an_invocation() {
    let scan = scan("for src in legion gh curl; do echo \"$src\"; done").expect("valid");
    assert!(invocation(&scan, "gh").is_none());
}

#[test]
fn gh_inside_a_quoted_argument_is_never_an_invocation() {
    let scan =
        scan("legion reflect --repo legion --text \"use gh via legion issue\"").expect("valid");
    assert!(invocation(&scan, "gh").is_none());
}

// -- Errors never produce a partial Scan ----------------------------------

#[test]
fn unterminated_single_quote_is_a_scan_error() {
    let err = scan("grep -n 'oops src").expect_err("unterminated quote");
    assert!(matches!(err, ScanError::UnterminatedSingleQuote { .. }));
}

#[test]
fn unterminated_dollar_single_quote_is_a_scan_error() {
    let err = scan("grep -n $'oops src").expect_err("unterminated $'...' quote");
    assert!(matches!(err, ScanError::UnterminatedSingleQuote { .. }));
}

#[test]
fn trailing_backslash_at_end_of_input_is_kept_literal() {
    let scan = scan("echo foo\\").expect("a lone trailing backslash is not an error");
    let echo = invocation(&scan, "echo").expect("echo resolved");
    assert_eq!(echo.args, vec!["foo\\"]);
}

#[test]
fn unterminated_quote_in_heredoc_delimiter_is_a_scan_error() {
    // Before the fix, the unclosed `'` inside the delimiter word consumed
    // the rest of the input looking for a closing quote that never came.
    let err =
        scan("bash <<'EOF\ngrep -rn foo src\nEOF\n").expect_err("unterminated delimiter quote");
    assert!(matches!(
        err,
        ScanError::UnterminatedSingleQuote { .. } | ScanError::MissingHeredocDelimiter { .. }
    ));
}

#[test]
fn unterminated_heredoc_is_a_scan_error() {
    let err = scan("bash <<'EOF'\nrg foo\n").expect_err("unterminated heredoc");
    assert!(matches!(err, ScanError::UnterminatedHeredoc { .. }));
}

#[test]
fn unterminated_substitution_is_a_scan_error() {
    let err = scan("echo $(grep foo").expect_err("unterminated substitution");
    assert!(matches!(err, ScanError::UnterminatedSubstitution { .. }));
}

#[test]
fn scan_never_panics_on_a_deterministic_fuzz_corpus() {
    // No proptest/quickcheck dependency: a small deterministic generator
    // (xorshift64, no multiply step) over an alphabet that includes every
    // construct this scanner special-cases (quotes, backticks, `$(`, `<<`,
    // braces, unbalanced parens, multibyte characters) stands in for a
    // property test (NFR requires "no panic on any input, including
    // invalid UTF-8 boundaries in slices").
    let alphabet: &[char] = &[
        'a',
        ' ',
        '\'',
        '"',
        '`',
        '\\',
        '$',
        '(',
        ')',
        '{',
        '}',
        '<',
        '>',
        '|',
        '&',
        ';',
        '\n',
        '\t',
        '-',
        '=',
        '.',
        '/',
        '*',
        '@',
        '#',
        '!',
        '~',
        'あ',
        '\u{1F600}',
    ];
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = || {
        // xorshift64: deterministic, dependency-free.
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..2000 {
        let len = (next() % 40) as usize;
        let s: String = (0..len)
            .map(|_| alphabet[(next() as usize) % alphabet.len()])
            .collect();
        // The only contract under test is "never panics"; either outcome
        // of the Result is acceptable.
        let _ = scan(&s);
    }
}

#[test]
fn scan_error_display_names_the_construct_and_offset() {
    let err = scan("grep -n 'oops src").expect_err("unterminated quote");
    assert!(err.to_string().contains("unterminated single quote"));
    assert!(err.to_string().contains("byte"));
}

// -- Scan::is_single_simple_command (FR-CMD-008): a rewrite replaces the
// whole command string, so route only rewrites when the invocation it
// targets IS the whole command -- no operator, redirect, substitution,
// heredoc, or opaque sibling part.

#[test]
fn a_bare_command_is_a_single_simple_command() {
    let scan = scan("gh issue list").expect("valid command");
    assert!(scan.is_single_simple_command);
}

#[test]
fn a_pipeline_is_not_a_single_simple_command() {
    let scan = scan("gh issue list | head").expect("valid command");
    assert!(!scan.is_single_simple_command);
}

#[test]
fn an_and_and_compound_is_not_a_single_simple_command() {
    let scan = scan("cd x && gh issue list").expect("valid command");
    assert!(!scan.is_single_simple_command);
}

#[test]
fn a_semicolon_compound_is_not_a_single_simple_command() {
    let scan = scan("gh issue list; echo done").expect("valid command");
    assert!(!scan.is_single_simple_command);
}

#[test]
fn a_trailing_semicolon_alone_is_still_a_single_simple_command() {
    // A lone trailing `;` (or newline) ends the command with no other
    // effect -- semantically identical to the bare command -- unlike a
    // trailing `&`, which backgrounds it.
    let scan = scan("gh issue list;").expect("valid command");
    assert!(scan.is_single_simple_command);
}

#[test]
fn a_trailing_background_operator_is_not_a_single_simple_command() {
    // The group count alone cannot catch this: nothing follows the `&`,
    // so there is still only one top-level group, but backgrounding
    // changes what the command does -- a rewrite that drops it is not
    // lossless.
    let scan = scan("gh issue list &").expect("valid command");
    assert!(!scan.is_single_simple_command);
}

#[test]
fn a_redirect_is_not_a_single_simple_command() {
    // The redirect target is dropped by the tokenizer (never kept as a
    // word or an invocation), so this can only be caught by the
    // dedicated `has_redirect` trace -- an invocation-count check alone
    // would miss it.
    let scan = scan("gh issue list > out.txt").expect("valid command");
    assert!(!scan.is_single_simple_command);
}

#[test]
fn an_input_redirect_is_not_a_single_simple_command() {
    let scan = scan("grep foo < input.txt").expect("valid command");
    assert!(!scan.is_single_simple_command);
}

#[test]
fn a_command_substitution_is_not_a_single_simple_command() {
    let scan = scan("echo $(rm -rf /)").expect("valid command");
    assert!(!scan.is_single_simple_command);
}

#[test]
fn a_command_with_an_opaque_sibling_is_not_a_single_simple_command() {
    let scan = scan("grep -rn foo | ./notify.sh").expect("valid command");
    assert!(!scan.is_single_simple_command);
}

#[test]
fn a_wrapper_around_a_single_command_is_not_a_single_simple_command() {
    // `record` (this module) records an invocation for every resolved
    // command position, including a wrapper's own (`sh` here, not just
    // the `grep` it wraps) -- so `sh -c '...'` yields two invocations,
    // not one. Conservative, not a bug: nothing requires a wrapped
    // command to be rewrite-eligible, and failing closed here never lets
    // an unsafe rewrite through.
    let scan = scan("sh -c 'grep -rn foo src'").expect("valid command");
    assert_eq!(scan.invocations.len(), 2);
    assert!(!scan.is_single_simple_command);
}

#[test]
fn an_empty_command_is_not_a_single_simple_command() {
    let scan = scan("").expect("empty command parses");
    assert!(!scan.is_single_simple_command);
}

#[test]
fn a_stderr_redirect_with_no_target_word_is_not_a_single_simple_command() {
    // `>&2` never reaches the `Tok::Word` branch that used to be
    // `has_redirect`'s only source -- it must still be caught.
    let scan = scan("gh issue list >&2").expect("valid command");
    assert!(!scan.is_single_simple_command);
}

#[test]
fn a_digit_glued_redirect_is_not_a_single_simple_command() {
    let scan = scan("gh issue list 2>/dev/null").expect("valid command");
    assert!(!scan.is_single_simple_command);
}

#[test]
fn an_assignment_prefix_is_not_a_single_simple_command() {
    // `FOO=1` is part of the command string a rewrite would replace, but
    // it is not visible in the invocation's own `args` -- so it would be
    // silently dropped, the same loss a redirect or operator causes.
    let scan = scan("FOO=1 gh issue list").expect("valid command");
    assert!(!scan.is_single_simple_command);
}
