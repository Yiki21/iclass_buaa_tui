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
| `resource_unavailable` | seat/room taken or not bookable | pick another |
| `account_locked` | upstream locked the account | wait for the lock window to expire |
| `rate_limited` | too many requests | back off |
| `upstream_timeout`, `network_error`, `upstream_error` | transient | retry with backoff |
| `unknown` | not classified | treat as non-retryable |

Exit codes are only 0 (success) and 1 (failure). The exit code does not
distinguish kinds of failure; `code` does.

## Writes and their gates

Classified by what cannot be undone:

| command | effect | confirm flag |
|---|---|---|
| `venue-reserve` | write | `--yes` |
| `seat-book` | write | `--yes` |
| `sign` | write | `--yes` |
| `plan` | write | `--yes` |
| `install-autologin`, `uninstall-autologin` | write | `--yes` |
| `clockin-submit` | **irreversible** | `--yes` |
| `eval-submit` | **irreversible** | `--yes` |

Irreversible means nothing in this tool undoes it. A booking or reservation can
be cancelled (`venue-orders --cancel`, `seat-orders --cancel`); a clock-in
record and a submitted evaluation cannot.

`eval-submit` answers a questionnaire on the user's behalf and is attributed to
them. Do not call it without an explicit human instruction, and read
`eval --show <rwid>` first if you want to know what would be submitted.

## Order of operations

1. `doctor --json` — reachability of each upstream service, before anything else.
2. `list-today --json` — establishes a session, which everything else reuses.
3. Reads: `today`, `exams`, `grades --all`, `tasks`, `venues`, `seats`, `clockin`, `eval`.
4. Writes, last, and only with an explicit instruction.

## Things that are not obvious

- Most commands log in on their own, so a session failure surfaces as
  `not_authenticated` mid-run rather than at the start. One `list-today` first
  avoids that.
- `clockin-submit` requires `--photo` pointing at a real image; the service
  rejects a submission without one.
- The TUI is a separate mode and is not scriptable. Do not launch the binary
  without a subcommand from an agent: it takes over the terminal.
