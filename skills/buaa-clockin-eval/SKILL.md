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

- `eval --json`: the courses awaiting evaluation, each with `id`, `course`,
  `teacher`, `evaluated`. **Use `id`, not `rwid`, to name a course**: one
  evaluation round covers every course of the term, so `rwid` is the same for
  all of them.
- `eval --show <task> --json`: the questionnaire and the answers that would be
  sent. A course is a `task`; pass its `id`, or its `rwid` when only one course
  uses it.

Submit:

```bash
iclass_buaa_tui eval-submit --task <task> --json        # preview one
iclass_buaa_tui eval-submit --task <task> --yes --json  # submit one
iclass_buaa_tui eval-submit --all --yes --json          # every unevaluated course
```

The tool answers every choice question with its first option, which is
conventionally the most favourable; one question gets its second option, so the
answers are not uniform. Free-text questions ("优秀之处" / "不足之处") are left
blank. `eval-submit` prints the chosen option by its label ("优秀", "良好", ...),
not by id, so the preview reads like the questionnaire.

Submitted answers are recorded as the user's own opinion of the teacher. Before
`--yes`, tell the user which courses will be submitted and that the answers are
the tool's defaults; run `eval --show` so they can read the questions. Use
`--all` only when they asked for all of them.
