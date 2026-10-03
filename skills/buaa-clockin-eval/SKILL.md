---
name: buaa-clockin-eval
description: BUAA 阳光打卡 (sunshine sports clock-in) and 评教 (course evaluation) via `iclass_buaa_tui`. Both submissions are irreversible. Load after `buaa-campus` when the user asks about clock-in progress or records, wants to submit a clock-in, or asks about pending course evaluations.
---

# 阳光打卡 and 评教

The two writes here, `clockin-submit` and `eval-submit`, are `write_irreversible`: nothing in this tool or the school's systems undoes them. Read freely; submit only on a direct instruction from the user for that specific submission, and only after showing them the preview.

## 阳光打卡

Reads:

- `clockin --json`: `classifies` (categories: `id`, `name`, required `term_num` / `week_num` / `month_num`), `selected` (the category shown), `items` (`id`, `name`), and `count` (progress).
- `clockin --classify <id> --records --json`: also the most recent records.

Submit:

```bash
iclass_buaa_tui clockin-submit --classify <id> --item <id> --photo <path> \
  --place 操场 --start "2026-03-10 07:00" --end "2026-03-10 07:40" --json        # preview
# same command with --yes to submit
```

`--photo` must be a real image the user provides; the service rejects submissions without one. Never generate, fabricate, or reuse an image to stand in for one. Times are Beijing time, `YYYY-MM-DD HH:MM`; they default to the last 40 minutes. Record only activity the user says they actually did, with the times they give.

## 评教

Reads:

- `eval --json`: tasks with `rwid`, `course`, `teacher`, `evaluated`.
- `eval --show <rwid> --json`: the questionnaire and the answers that would be sent. The tool answers each question with its first option, which is conventionally the most favourable.

Submit:

```bash
iclass_buaa_tui eval-submit --task <rwid> --json        # preview one
iclass_buaa_tui eval-submit --task <rwid> --yes --json  # submit one
iclass_buaa_tui eval-submit --all --yes --json          # every unevaluated course
```

Submitted answers are recorded as the user's own opinion of the teacher. Before `--yes`, tell the user which courses will be submitted and that every answer will be the first option; run `eval --show` if they want to see the questions. Use `--all` only when they asked for all of them.
