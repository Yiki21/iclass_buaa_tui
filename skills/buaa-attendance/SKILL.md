---
name: buaa-attendance
description: iClass and 博雅 sign-in/sign-out (签到/签退) via `iclass_buaa_tui`, plus the scheduled autologin that signs automatically. Load after `buaa-campus` when the user asks to sign a class, check what still needs signing today, or set up / inspect / remove automatic signing.
---

# Attendance

## See what is due

`list-today --json` logs in and returns today's sign targets after the config's include/exclude filters:

```json
[{"source": "iclass", "action": "sign_in", "name": "...", "course_id": "...", "target_id": "...",
  "date": "2026-03-10", "start_time": "08:00", "end_time": "09:35", "signed": false}]
```

`target_id` is the id `sign` takes. For `iclass` it is the `course_sched_id`; for `bykc` it is the course id.

## Sign one target

Preview, then confirm:

```bash
iclass_buaa_tui sign --source iclass --course-sched-id <target_id> --json
iclass_buaa_tui sign --source iclass --course-sched-id <target_id> --yes --json

iclass_buaa_tui sign --source bykc --bykc-course-id <target_id> --action sign-in --yes --json
iclass_buaa_tui sign --source bykc --bykc-course-id <target_id> --action sign-out --yes --json
```

`sign` retries with a fresh login per attempt (`--retry-count`, `--retry-interval-seconds` override the config). BYKC signing needs `use_vpn = true` in the config and only succeeds inside the course's sign window; `bykc-detail --course <id> --json` shows the window (`sign_config`).

Sign only targets the user actually attends. Signing is an attendance record in their name.

## One automation cycle

`plan --json` (no `--yes`) evaluates every target today and reports which would be signed now. `plan --dry-run --json` evaluates without logging in to sign. `plan --yes --json` signs whatever is due. A lock prevents two cycles running at once for the same account; "planner 已在运行" means another cycle holds it.

## Scheduled autologin

| step | command |
|---|---|
| preview what would be installed | `install-autologin` |
| install the systemd timer / launchd / Task Scheduler entry | `install-autologin --yes` |
| check it is healthy | `autologin-status` |
| remove it | `uninstall-autologin --yes` |

`--planner-time HH:MM:SS` and `--planner-interval-minutes N` override the config for the generated entry. Installing changes the user's system scheduler, so confirm with the user first.

Failures during unattended runs surface as desktop notifications. `notify` sends a test notification to confirm this machine can show them.
