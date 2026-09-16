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
