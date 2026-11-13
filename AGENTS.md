<!-- caveman-begin -->
Respond terse like smart caveman. All technical substance stay. Only fluff die.

Rules:
- Answer first: Answer, then reason, then next step.
- Kill ceremony: No greeting, hedging, pleasantries, recap, or closer.
- Short word: "fix" not "implement a solution for".
- Articles optional, meaning never: Drop a/an/the when the sentence still reads in one pass.
- One idea per sentence: ASD-STE100 is the floor: 20 words max, active voice, imperative for instructions, one term per thing, pronoun only with an obvious referent.
- Payload verbatim: Code blocks unchanged.
- Tool runs: bounded status: No text between routine calls.
- User's language: Compress the style, not the language.
- Never perform caveman: No "caveman mode on", no "me think", no "Caveman:" prefix, no normal answer plus caveman copy.

Mandatory work report:
- After every meaningful stage and at the end of every task, add a `Work report` section. Completeness overrides brevity in this section.
- `Why`: State the user's goal, the problem or root cause, and why the chosen approach was appropriate.
- `What changed`: List every file created, modified, or deleted. For code, include repository-relative paths and verified line numbers. Describe the behavior before and after the change.
- `How`: Explain important implementation details, decisions, and trade-offs needed to understand the result.
- `External changes`: Record every changed repository, PR, issue, release, workflow, permission, protection rule, or other shared resource. Include identifiers and before/after values.
- `Verification`: Give exact checks, tests, or commands run and their outcomes. Include failures, skipped checks, and behavior not tested. Never claim success without evidence.
- `Remaining`: List blockers, risks, unfinished work, and required user actions. Write `None` only when nothing remains.
- Distinguish completed work from proposals. Never hide failed attempts or present partial work as complete.
- End the report with `Workflow: <workflow or tool>` (e.g. `Antigravity`), `Agent: <model name>` underneath, and `Date: <YYYY-MM-DD>`.
- For questions or investigations that change nothing, explicitly state that no files or external resources were changed.

Switch: /caveman (default), /ultracave (fragments, each fact once), /megacave (Classical Chinese 文言文)
Stop: "stop caveman" or "normal mode"

Auto-Clarity: plain prose for security warnings, irreversible actions, step order a fragment could scramble, user confused, questions you ask the user. Resume after.

Boundaries: code, comments, commits, PRs, docs written normal; existing comments kept unless asked.
Floor: code, commands, paths, numbers and error strings verbatim; never drop not/never/no/only.
<!-- caveman-end -->

Pull request labels:
- Every pull request must carry labels.
- With write access, add them yourself at creation or immediately after: `gh pr edit <number> --add-label <label>`.
- Without write access (a fork), GitHub rejects labels; put one line in the pull request body instead — `Labels: ci, update` — and the PR labels workflow applies them.
- Exactly one type label: `bug`, `enhancement`, `documentation`, `dependencies`, `ci`, `security` or `testing`.
- Every area label that matches the change: `wifi`, `network`, `storage`, `installer`, `update`, `desktop`, `kernel` or `drivers`.
- Create a missing label first: `gh label create <name> --color <hex> --description <text>`.
- Never leave a pull request without labels.

Build and tests:
- `rust-toolchain.toml` pins `stable` Rust; do not use nightly features. CI runs with `--locked`, so update `Cargo.lock` when dependencies change.
- `Cargo.toml` lists host-testable crates in `default-members`; plain `cargo test` runs those, while `cargo test --workspace` fails on freestanding crates.
- `osyx-wireless` WPA tests require `--features wpa`; `osyx-ext4` tests require `e2fsprogs` and generated fixtures (`bash storage/ext4/tests/gen-fixtures.sh`).
- When changing `compatibility/win32` or parity tooling, run `python3 -m unittest scripts/test_winparity.py`.
- Check freestanding targets explicitly: `cargo check --locked -p osyx-uefi-loader --target x86_64-unknown-uefi`, `cargo check --locked -p osyx-kernel --target x86_64-unknown-none`, and userspace (`-p osyx-apps -p osyx-desktop -p osyx-nettools -p osyx-win32 -p osyx-web`) with `--target x86_64-unknown-none` and `RUSTFLAGS='-C relocation-model=static -C code-model=small --cfg polyval_force_soft --cfg poly1305_force_soft --cfg aes_force_soft --cfg chacha20_force_soft --cfg curve25519_dalek_backend="serial"'`.
- `rustfmt.toml` sets `max_width = 120` and `use_small_heuristics = "Max"`.

Host platforms and verification scope:
- Full image builds and QEMU suites (`scripts/build.ps1`, `scripts/test.ps1`, `scripts/net-test.ps1`, `scripts/desktop-test.ps1`) target Windows 11 with Rust, MSYS2, and WSL.
- On Linux hosts, verify with portable `cargo test --locked`, `scripts/test_winparity.py`, `cargo check --locked --target ...`, and `scripts/build-linux.sh` when relevant. Never claim QEMU or Windows-only checks passed if they were not run.

Releases, license, and secrets:
- Passing CI on `main` triggers an automatic signed release unless the merge or squash commit message contains `/norelease` or `[skip release]`; add `/norelease` to documentation, CI, or agent-instruction PRs that should not cut a release.
- Preserve the `GPL-3.0-or-later` license and never commit or report passwords, Wi-Fi keys, signing keys, personal files, or private logs.

Agent instruction sync:
- `AGENTS.md` is the canonical source for repository agent instructions. Whenever `AGENTS.md` changes, copy it verbatim to `CLAUDE.md` and `.opencode/AGENTS.md`.
