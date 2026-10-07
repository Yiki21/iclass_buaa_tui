---
name: buaa-campus
description: Drive the `iclass_buaa_tui` CLI for BUAA (北航) campus services. Use when the user asks about their 课表/成绩/考试/空教室/作业, iClass or 博雅 sign-in (签到), 博雅课程选课, 研讨室 or 图书馆座位 booking, 阳光打卡, or 评教. Start here; it routes to the domain skill.
---

# BUAA campus CLI

`iclass_buaa_tui` is one binary. With a subcommand it is a scriptable CLI; with none it launches a full-screen TUI that takes over the terminal. Always pass a subcommand.

## First steps, every session

1. `iclass_buaa_tui schema` prints every command as JSON: arguments, `effect` (`read` / `write` / `write_irreversible`), `confirmation_flag`, `supports_json`. A command that writes only with a flag has `write_when` set to that flag and a second entry named after it (e.g. `venue-orders --cancel`), so gate on `effect` even for a command you normally use to read. It comes from the parser, so trust it over this skill when they disagree.
2. `iclass_buaa_tui doctor --json` checks reachability of WebVPN, SSO, iClass and BYKC. Run it before blaming credentials.
3. Pass `--json` on every call that supports it and parse stdout.

Each invocation logs in on its own with its own cookie jar. Credentials come from the config file (`$XDG_CONFIG_HOME/iclass-buaa/config.toml`, or `--config <path>`). Never ask the user for their password in chat; if login fails with `config_invalid`, tell them to edit the config file themselves.

## The write rule

Every command whose `effect` is not `read` does nothing without `--yes`. Without it the call exits 0 and prints a preview with `"submitted": false`. Read `submitted` from the JSON; the exit code alone does not tell you whether anything was written.

Work in two calls: run the write without `--yes` to preview, show the user what it would do, then repeat with `--yes` only after the user has explicitly agreed to that specific action. A preview re-reads live state, so it also tells you whether the write would still succeed.

`write_irreversible` commands (`clockin-submit`, `eval-submit`) have no undo anywhere. Run them only on a direct, specific instruction from the user for that exact submission.

## Failures

With `--json`, a failure prints to stderr:

```json
{"error": {"message": "...", "causes": ["..."]}, "command": "...", "retryable": false, "code": "..."}
```

Act on `code`:

| code | do |
|---|---|
| `not_authenticated` | retry the command once; if it fails again, run `doctor --json` and report |
| `config_invalid` | stop; the user fixes the config file (it must be mode 600 when it holds a password) |
| `invalid_argument` | fix the arguments from `schema`; do not retry unchanged |
| `resource_unavailable` | the seat/room/course is taken by someone else or closed; offer an alternative |
| `already_booked` | the user already holds a booking in that slot; offer to cancel it or pick another slot/date — do not retry |
| `account_locked` | stop; the SSO lock clears on its own after a while |
| `rate_limited` | back off at least a minute |
| `connect_timeout`, `read_timeout` | retry with backoff; a write that timed out may have landed, so re-read before repeating it |
| `upstream_timeout`, `network_error`, `upstream_error` | retry with backoff, at most 3 times |
| `unknown` | treat as non-retryable and show the message |

Exit codes are only 0 and 1.

## Domains

Load the matching skill before acting in its area:

| skill | covers |
|---|---|
| `buaa-academics` | `today`, `schedule-export`, `schedule-diff`, `exams`, `grades`, `classrooms`, `tasks` |
| `buaa-attendance` | `list-today`, `sign`, `plan`, `install-autologin`, `autologin-status`, `uninstall-autologin`, `notify` |
| `buaa-bykc` | 博雅课程: `bykc-courses`, `bykc-chosen`, `bykc-detail`, `bykc-stats`, `bykc-select`, `bykc-deselect` |
| `buaa-booking` | 研讨室 `venues`, `venue-slots`, `venue-reserve`, `venue-orders`; 图书馆 `seats`, `seat-map`, `seat-book`, `seat-orders` |
| `buaa-clockin-eval` | 阳光打卡 `clockin`, `clockin-submit`; 评教 `eval`, `eval-submit` |

The skills ship inside the binary and match its version. If a domain skill is not installed, `skills show <name>` prints it; `skills list --json` lists them; `skills install --target <skills dir> --yes` writes all of them (it refuses to overwrite files it did not write).

## Dates and time

Dates are `YYYY-MM-DD`; times are Beijing time. Resolve relative dates ("明天", "next Friday") to an absolute date yourself and state it back to the user before any write.
