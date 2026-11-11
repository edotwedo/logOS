# AGENTS.md: how an AI contributes to logOS

logOS is built in the open by many people's AI assistants at once. Whatever model you are
and whoever you work for, these rules are the same for everyone. Humans follow them too.

## Before you start

1. Read [README.md](README.md), especially the requirements R1–R6 and the roadmap.
2. Pick one roadmap milestone or one open issue. Comment on the issue so two agents don't
   build the same thing.
3. Build it: `rustup target add x86_64-unknown-uefi && cargo build --release`.
   Boot it: `./run_qemu.sh` (needs QEMU and OVMF).

## The rules

- **Small pull requests.** One idea per PR. A reviewer should be able to read it in ten minutes.
- **It must boot.** CI builds every PR and boots it in QEMU. A PR that doesn't boot doesn't merge.
- **No hidden state.** No network calls, telemetry or downloads at build or boot time unless the
  milestone is about exactly that, and the PR says so in its first line.
- **Drivers are fenced off (R3).** A driver touches only the device it was given. No driver
  writes to memory it wasn't granted, and a driver that fails must not take the system down.
- **Prove it (R4).** Add a test, a QEMU boot check, or a recorded hardware trace that shows the
  change works. Say in the PR how you checked it.
- **Say who wrote it (R6).** End the PR description with the model and tool that wrote the code
  (for example `Written by: Claude Code` or `Written by: Codex`) and the human who sent it.
- **A human approves.** AI drafts, a maintainer merges. Never merge your own PR.
- **No secrets, no copied code.** Don't commit keys or tokens. Don't paste code from projects
  whose license doesn't allow MIT/Apache-2.0 reuse; write it fresh and cite the spec you used.
- **Be honest in the README.** Only mark a milestone done when CI proves it.

## Good first tasks

- Make the QEMU boot test check more of the boot log.
- M6: write the A/B slot layout and a signature check for the next image (start in an emulator).
- M7: design the driver task boundary: what a driver is allowed to see, and how it's restarted.
- Docs: explain one part of `src/` for the next contributor.
