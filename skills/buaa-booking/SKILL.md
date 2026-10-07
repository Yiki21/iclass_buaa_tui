---
name: buaa-booking
description: Book and cancel BUAA 研讨室 (seminar rooms) and 图书馆座位 (library seats) via `iclass_buaa_tui`. Load after `buaa-campus` when the user wants to find a free room or seat, reserve one, list their reservations, or cancel one.
---

# Booking rooms and seats

Both flows are: discover ids, check availability, preview the write, then confirm with `--yes`. Bookings are cancellable, but they hold a shared resource, so confirm the exact room or seat, date and time with the user before `--yes`.

## Seminar rooms (研讨室)

1. `venues --json [--query 沙河]`: rooms as `{id, site_name, venue_name, campus_name, seat_count}`. `id` is `--site`.
2. `venue-slots --site <id> --date YYYY-MM-DD --json`: `time_slots` (`id`, `label`) and per-space `slots` with `reservable` / `take_up`. Pick slot ids where `reservable` is true and `take_up` is false.
3. Preview:

   ```bash
   iclass_buaa_tui venue-reserve --site 7 --date 2026-03-10 --slots 1,2 \
     --phone <phone> --theme 小组讨论 --json
   ```

   The preview re-reads availability and reports whether the slots are still free.
4. Same command with `--yes`. Success returns `"submitted": true` and an `order_id`.

Required: `--phone` (the user's contact number; ask, never invent one) and `--theme` (short title). Optional: `--joiners N`, `--joiner-names a,b`, `--activity`, `--purpose <id>`. Purpose ids are opaque numbers; run `venues` without `--json` to see the `用途: 1=... 2=...` line. The default `1` is usually fine.

List and cancel:

1. `venue-orders --json`: each order has `id` (a number), `space_name`, `reservation_date`, `start_date`, `end_date`, `theme`, `order_status` and `status_text`.
2. Preview: `venue-orders --cancel <order id> --json` returns `"submitted": false` with `would_cancel`, `cancellable` and `blocked_reason`. An id that is not among the 20 most recent orders is refused before anything is sent.
3. Confirm with the user, then the same command with `--yes`. Success returns `"submitted": true` and `cancelled`.

## Library seats (图书馆座位)

1. `seats --date YYYY-MM-DD --json`: `libraries` with `id`, `free_num`, `total_num`.
2. `seats --library <library id> --date ... --json`: adds `areas` (`id` is `--area`).
3. `seat-map --area <area id> --date ... --free --json`: `segments` (time segment ids), `available_dates`, and `seats` with `id`, `no`, `is_available`, and `x`/`y` (position on the floor plan, in percent) when the area has a plan. `--segment <id>` picks a segment; it defaults to the first.
4. Preview: `seat-book --area <area> --seat <seat id> --date ... [--segment <id>] --json`.
5. Same command with `--yes`. Success returns `"submitted": true` and a `booking_id`.

`--seat` takes the seat `id`, not the printed seat number `no`. When the user names a seat by its number, map it through `seat-map` first.

List and cancel:

1. `seat-orders --json`: each booking has `id`, `day`, `begin_time`, `end_time`, `status`, `status_name` and `cancellable`. `id` and `status` are strings (`"393964"`, `"6"`), not numbers; compare them as strings. Decide on `cancellable`, not on `status`: only `cancellable: true` bookings can be cancelled.
2. Preview: `seat-orders --cancel <booking id> --json` returns `"submitted": false` with `would_cancel`, `cancellable` and `blocked_reason`.
3. Confirm with the user, then the same command with `--yes`. Success returns `"submitted": true`.

Cancelling gives up a seat that may not be free again. Do it only when the user asks for that booking by name.

## When it is taken

A `resource_unavailable` failure means someone else got it first or the window closed. Re-read availability and offer the nearest alternative; do not retry the same id.

An `already_booked` failure is different: the user already holds a booking in that slot (the library says `不可重复预约`), so every seat in it is refused. Do not retry and do not offer another seat in the same slot; offer to cancel the existing booking (find it with `seat-orders --json` or `venue-orders --json`) or to pick another slot or date.
