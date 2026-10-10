# Notes for automated callers

This file is for agents driving `iclass_buaa_tui` from a script or an LLM tool
loop. It assumes you will call the binary, not the library.

## Discover the interface, do not guess it

```
iclass_buaa_tui schema
```

Prints every command as JSON: its arguments, whether it reads or writes, the
flag that confirms a write, and whether it supports `--json`. Arguments come
from the parser itself, so they cannot be out of date. Prefer this over
scraping `--help`.

## Skills

The binary carries Agent Skills (SKILL.md files) that describe each domain's
workflow: which read gives the id the next write needs, and what to confirm
with the user first. They match the binary's version.

```
iclass_buaa_tui skills list --json            # names and descriptions
iclass_buaa_tui skills show buaa-campus       # print one; start with this router
iclass_buaa_tui skills install --target ~/.claude/skills --yes
```

`install` refuses to overwrite a file it did not write, and touches nothing if
any file conflicts. `--force` updates files from an earlier install. Sources
live in `skills/`.

## One rule that will bite you

**Every command that writes does nothing without `--yes`.** Without it the call
succeeds, exits zero, and reports a preview with `"submitted": false`.

That means a write command whose exit status is 0 has not necessarily written
anything. Always read `submitted` from the `--json` output rather than trusting
the exit code:

```bash
iclass_buaa_tui venue-reserve --site 7 --date 2026-03-10 --slots 1,2 \
  --phone 13800000000 --theme 讨论 --json
# -> {"submitted": false, "would_reserve": {...}}   exit 0, nothing booked

iclass_buaa_tui venue-reserve ... --yes --json
# -> {"submitted": true, "order_id": 12, ...}
```

The preview is useful on its own: it re-reads availability and reports whether
the slots are still free, so the same command can plan and then execute.

## Reading failures

With `--json`, a failure also writes a structured report to stderr:

```json
{
  "error": { "message": "...", "causes": ["..."] },
  "command": "venue-reserve",
  "retryable": false,
  "code": "not_authenticated"
}
```

`retryable` is the field to act on. `code` values:

| code | meaning | what to do |
|---|---|---|
| `not_authenticated` | session expired | run `list-today` once to log in again |
| `config_invalid` | missing or unreadable config | check `--config`, file permissions |
| `invalid_argument` | the command line is wrong | fix the arguments, do not retry |
| `resource_unavailable` | seat/room taken by someone else, or not bookable | pick another |
| `already_booked` | you already hold a booking in that slot | cancel the existing one, or pick another slot/date; do not retry |
| `account_locked` | upstream locked the account | wait for the lock window to expire |
| `rate_limited` | too many requests | back off |
| `connect_timeout`, `read_timeout` | the request timed out connecting / reading | retry with backoff |
| `upstream_timeout`, `network_error`, `upstream_error` | transient | retry with backoff |
| `unknown` | not classified | treat as non-retryable |

The table is the contract `src/failure.rs` implements; a keyword that misses
means a call reports `unknown` where a specific code was promised.

Exit codes are only 0 (success) and 1 (failure). The exit code does not
distinguish kinds of failure; `code` does.

## Writes and their gates

Classified by what cannot be undone. Confirm the flag exists before passing it:
`schema` lists the real one per command.

| command | effect | confirm flag |
|---|---|---|
| `venue-reserve` | write | `--yes` |
| `venue-orders --cancel` | write | `--yes` |
| `seat-book` | write | `--yes` |
| `seat-orders --cancel` | write | `--yes` |
| `sign` | write | `--yes` |
| `plan` | write | `--yes` |
| `bykc-select` | write | `--yes` |
| `bykc-deselect` | write | `--yes` |
| `install-autologin`, `uninstall-autologin` | write | `--yes` |
| `skills install` | write | `--yes` |
| `clockin-submit` | **irreversible** | `--yes` |
| `eval-submit` | **irreversible** | `--yes` |

`venue-orders` and `seat-orders` are reads until `--cancel` names an order, at
which point they are writes; `schema` lists each form separately, with `effect`
and `write_when` saying which flag turns the read into the write.

Irreversible means nothing in this tool undoes it. A booking or reservation can
be cancelled (`venue-orders --cancel`, `seat-orders --cancel`); a BYKC
enrollment can be withdrawn with `bykc-deselect` until the course's cancel
deadline; a clock-in record and a submitted evaluation cannot be undone.

`clockin-submit` and `eval-submit` write a record that is attributed to the real
account. Their previews say so; `eval-submit` additionally answers a
questionnaire on the user's behalf. Do not call either without an explicit human
instruction for that exact submission, and for `eval-submit` read
`eval --show <task>` first if you want to know what would be submitted. `eval`
and `eval-submit` identify a course by the `task` id it prints, not by `rwid`:
one evaluation round (`rwid`) covers every course of the term.

## Order of operations

1. `doctor --json` — reachability of each upstream service, before anything else.
2. `list-today --json` — verifies authentication for that process. Each CLI invocation creates its own in-memory cookie jar; a previous command does not establish a session for a later process.
3. Reads: `today`, `exams`, `grades --all`, `tasks`, `venues`, `seats`, `seat-map`, `clockin`, `eval`.
4. Writes, last, and only with an explicit instruction. `venue-orders` and
   `seat-orders` count as writes too once `--cancel` is passed.

## Things that are not obvious

- Most commands log in on their own. A successful `list-today` does not share
  cookies with a later command, so it is a diagnostic step rather than a
  cross-process login bootstrap.
- `clockin-submit` requires `--photo` pointing at a real image; the service
  rejects a submission without one.
- The TUI is a separate mode and is not scriptable. Do not launch the binary
  without a subcommand from an agent: it takes over the terminal.
