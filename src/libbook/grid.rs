//! Floor-plan layout for library seat maps.
//!
//! Why:
//! The reference client lists seats four to a row in the order the service
//! returns them, which loses the shape of the room: aisles, table blocks and
//! window rows all disappear. The `Space/seat` reply carries each seat's
//! position on the area's floor plan (`point_x`, `point_y`, percentages of the
//! plan image), so a terminal can draw the real layout instead.
//!
//! How:
//! Positions are continuous and a terminal is a grid of cells, so each axis is
//! quantized on its own. The typical spacing between neighbouring seats on one
//! line (the pitch) decides which positions share a column or row, and a gap
//! wider than one and a half pitches leaves one empty cell so aisles stay
//! visible without stretching the map. When no seat (or only a minority) has
//! a position the area has no plan, and seats are laid out by their numbering
//! instead. When most do, the plan is drawn and the few without a position
//! are listed by number below it, after an empty row.

use std::collections::HashSet;

use super::Seat;

/// Which source the layout was built from.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]

pub enum Layout {
    /// Built from the floor-plan coordinates the service returned. Seats
    /// without coordinates, if any, follow below the plan; see
    /// [`SeatGrid::unplaced`].
    FloorPlan,
    /// No coordinates; seats follow their numbering in fixed-width rows.
    #[default]
    Numbering,
}

/// A direction of movement on the map.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]

pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// Seats placed on a grid of terminal cells.

#[derive(Clone, Debug, Default, PartialEq, Eq)]

pub struct SeatGrid {
    pub layout:   Layout,
    pub rows:     usize,
    pub cols:     usize,
    /// Seats drawn below a floor plan because they had no coordinates. Zero
    /// for a complete plan and for a numbered layout.
    pub unplaced: usize,
    /// `(row, col)` of each seat, aligned with the seat list it was built from.
    positions:    Vec<(usize, usize)>,
    /// Seat index at each cell, row-major.
    cells:        Vec<Option<usize>>,
}

/// Seats per row when laying out by numbering.
///
/// Why 10:
/// Reading-room seats are numbered in runs (001-010, 011-020), so a row of ten
/// follows a numbering the service already encodes, and it still fits an
/// ordinary terminal with the seat number written in each cell.

pub const NUMBERING_ROW_WIDTH: usize = 10;

/// Positions closer than this fraction of the pitch share a cell line.

const MERGE_FRACTION: f64 = 0.5;

/// A gap wider than this many pitches is drawn as one empty cell.

const AISLE_PITCHES: f64 = 1.5;

/// Two seats whose cross-axis positions differ by less than this many plan
/// percent are treated as being on the same line when measuring the pitch.

const SAME_LINE_PERCENT: f64 = 0.5;

/// Positions closer than this are the same position, not a spacing sample.

const EPSILON: f64 = 0.05;

impl SeatGrid {
    /// Lays out `seats`: from their floor-plan coordinates when most of them
    /// have some, and from their numbering otherwise.
    ///
    /// Why not all-or-nothing:
    /// A single seat the service returns without a position used to turn a
    /// whole reading room into numbered rows. The plan of the other seats is
    /// still right, so it is kept, and the seats without a position are listed
    /// by number under it, after an empty row, where they cannot be mistaken
    /// for part of the room. When positions are the exception rather than the
    /// rule there is no plan worth drawing, and the numbered layout is used.

    pub fn build(seats: &[Seat]) -> Self {

        let (placed, unplaced): (Vec<usize>, Vec<usize>) =
            (0..seats.len()).partition(|&index| point(&seats[index]).is_some());

        if placed.is_empty() || placed.len() * 2 <= seats.len() {

            return Self::from_positions(Layout::Numbering, numbering(seats.iter()));
        }

        let points: Vec<(f64, f64)> = placed
            .iter()
            .filter_map(|&index| point(&seats[index]))
            .collect();

        let plan = floor_plan(&points);

        // One empty row between the plan and the seats listed under it.
        let below = plan.iter().map(|&(row, _)| row + 2).max().unwrap_or(0);

        let listed = numbering(unplaced.iter().map(|&index| &seats[index]));

        let mut positions = vec![(0, 0); seats.len()];

        for (&index, cell) in placed.iter().zip(plan) {

            positions[index] = cell;
        }

        for (&index, (row, col)) in unplaced.iter().zip(listed) {

            positions[index] = (below + row, col);
        }

        Self {
            unplaced: unplaced.len(),
            ..Self::from_positions(Layout::FloorPlan, positions)
        }
    }

    /// How the map was laid out, in words for the legend.
    ///
    /// Why here:
    /// A partial plan must not read as a complete one, and the grid is the
    /// only place that knows which kind it built.

    #[allow(dead_code)]

    pub fn caption(&self) -> &'static str {

        match (self.layout, self.unplaced) {
            (Layout::FloorPlan, 0) => "按平面图排列",
            (Layout::FloorPlan, _) => "按平面图排列，部分座位无坐标，按编号列在下方",
            (Layout::Numbering, _) => "无平面图，按编号排列",
        }
    }

    /// `(row, col)` of the seat at `index` in the seat list.

    pub fn position(&self, index: usize) -> Option<(usize, usize)> {

        self.positions.get(index).copied()
    }

    /// The seat index drawn at a cell, if any.

    pub fn at(&self, row: usize, col: usize) -> Option<usize> {

        if row >= self.rows || col >= self.cols {

            return None;
        }

        self.cells[row * self.cols + col]
    }

    /// Seat indices in reading order: row by row, left to right.

    pub fn reading_order(&self) -> impl Iterator<Item = usize> + '_ {

        self.cells.iter().flatten().copied()
    }

    /// The first seat after `from` in reading order that satisfies `keep`,
    /// wrapping past the end. `from` itself is considered last.

    pub fn next_where(&self, from: usize, keep: impl Fn(usize) -> bool) -> Option<usize> {

        let order: Vec<usize> = self.reading_order().collect();

        let start = order
            .iter()
            .position(|&index| index == from)
            .map_or(0, |at| at + 1);

        (0..order.len())
            .map(|step| order[(start + step) % order.len()])
            .find(|&index| keep(index))
    }

    /// The seat reached by moving from `from` in `direction`.
    ///
    /// How:
    /// Left and right walk the seats in reading order: to the neighbour on the
    /// same row, and past the end of a row on to the next row's first seat.
    /// Up and down look for the nearest seat strictly ahead, where sideways
    /// drift counts double, so moving down a column prefers the seat directly
    /// below two rows away over a diagonal neighbour one row away, which is
    /// what the eye expects on a map with aisles.
    ///
    /// Why left and right do not search the plane too:
    /// A nearest-ahead choice in all four directions is a local rule, and it
    /// can leave a seat that no neighbour ever picks: one sitting between
    /// others that each have a cheaper seat ahead of them in every direction.
    /// That seat is drawn, but no key reaches it. Reading order visits every
    /// seat, so with left and right following it each seat is reachable from
    /// every other, whatever up and down choose.

    pub fn step(&self, from: usize, direction: Direction) -> Option<usize> {

        let (row, col) = self.position(from)?;

        if matches!(direction, Direction::Left | Direction::Right) {

            let order: Vec<usize> = self.reading_order().collect();

            let at = order.iter().position(|&index| index == from)?;

            let next = match direction {
                Direction::Left => at.checked_sub(1)?,
                _ => at + 1,
            };

            return order.get(next).copied();
        }

        self.positions
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != from)
            .filter_map(|(index, &(other_row, other_col))| {

                let ahead = match direction {
                    Direction::Up => row as isize - other_row as isize,
                    _ => other_row as isize - row as isize,
                };

                let lateral = col.abs_diff(other_col);

                // Lazily: for a seat behind, `ahead as usize` would wrap.
                (ahead > 0).then(|| (ahead as usize + 2 * lateral, lateral, index))
            })
            .min()
            .map(|(_, _, index)| index)
    }

    /// Builds the cell table, moving a seat that lands on an occupied cell to
    /// the next free cell on its row.
    ///
    /// Why:
    /// Quantizing can, in principle, put two seats in one cell. Hiding one of
    /// them would make a real seat unselectable, so it is shifted instead.

    fn from_positions(layout: Layout, positions: Vec<(usize, usize)>) -> Self {

        let mut taken = HashSet::new();

        let positions: Vec<(usize, usize)> = positions
            .into_iter()
            .map(|(row, mut col)| {

                while !taken.insert((row, col)) {

                    col += 1;
                }

                (row, col)
            })
            .collect();

        let rows = positions.iter().map(|&(row, _)| row + 1).max().unwrap_or(0);

        let cols = positions.iter().map(|&(_, col)| col + 1).max().unwrap_or(0);

        let mut cells = vec![None; rows * cols];

        for (index, &(row, col)) in positions.iter().enumerate() {

            cells[row * cols + col] = Some(index);
        }

        Self {
            layout,
            rows,
            cols,
            unplaced: 0,
            positions,
            cells,
        }
    }
}

/// A seat's floor-plan position, when it has a usable one.

fn point(seat: &Seat) -> Option<(f64, f64)> {

    Some((seat.x?, seat.y?))
}

/// Quantizes floor-plan points into `(row, col)` cells.
///
/// Why borrow the other axis's pitch:
/// A single row of seats has no two seats in one column, so the row pitch has
/// no samples of its own. Falling back to the gaps between sorted positions
/// would then measure the hand-placement jitter (a few tenths of a percent)
/// and split one row into several. Seats are spaced alike in both directions,
/// so the other axis's pitch is the better estimate.

fn floor_plan(points: &[(f64, f64)]) -> Vec<(usize, usize)> {

    let xs: Vec<f64> = points.iter().map(|point| point.0).collect();

    let ys: Vec<f64> = points.iter().map(|point| point.1).collect();

    let col_pitch = line_pitch(&xs, &ys);

    let row_pitch = line_pitch(&ys, &xs);

    let col_pitch = col_pitch
        .or(row_pitch)
        .unwrap_or_else(|| sorted_gap_pitch(&xs));

    let row_pitch = row_pitch.unwrap_or(col_pitch);

    let cols = quantize(&xs, col_pitch);

    let rows = quantize(&ys, row_pitch);

    rows.into_iter().zip(cols).collect()
}

/// The typical spacing between neighbouring seats along one axis, or `None`
/// when no two seats share a line.
///
/// How:
/// For every seat, the nearest other seat on the same line (cross-axis
/// position within [`SAME_LINE_PERCENT`]) gives one spacing sample; the
/// median ignores the odd seat across an aisle.

fn line_pitch(along: &[f64], across: &[f64]) -> Option<f64> {

    let mut samples: Vec<f64> = (0..along.len())
        .filter_map(|index| {

            (0..along.len())
                .filter(|&other| {

                    other != index && (across[other] - across[index]).abs() < SAME_LINE_PERCENT
                })
                .map(|other| (along[other] - along[index]).abs())
                .filter(|distance| *distance > EPSILON)
                .min_by(f64::total_cmp)
        })
        .collect();

    median(&mut samples)
}

/// The median gap between sorted positions, for a map where no two seats
/// share a line on either axis.

fn sorted_gap_pitch(along: &[f64]) -> f64 {

    let mut sorted = along.to_vec();

    sorted.sort_by(f64::total_cmp);

    let mut gaps: Vec<f64> = sorted
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .filter(|gap| *gap > EPSILON)
        .collect();

    median(&mut gaps).unwrap_or(1.0)
}

fn median(values: &mut [f64]) -> Option<f64> {

    if values.is_empty() {

        return None;
    }

    values.sort_by(f64::total_cmp);

    Some(values[values.len() / 2])
}

/// Maps positions on one axis to cell indexes, aligned with the input.

fn quantize(values: &[f64], pitch: f64) -> Vec<usize> {

    let mut order: Vec<usize> = (0..values.len()).collect();

    order.sort_by(|left, right| values[*left].total_cmp(&values[*right]));

    let mut cells = vec![0; values.len()];

    let Some(&first) = order.first() else {

        return cells;
    };

    let mut cell = 0;

    let mut anchor = values[first];

    let mut previous = anchor;

    for &index in &order {

        let value = values[index];

        if value - anchor > pitch * MERGE_FRACTION {

            cell += if value - previous > pitch * AISLE_PITCHES {

                2
            } else {

                1
            };

            anchor = value;
        }

        cells[index] = cell;

        previous = value;
    }

    cells
}

/// Lays seats out in service order, [`NUMBERING_ROW_WIDTH`] per row.
///
/// How:
/// A number that does not continue the previous one (a new block, or a label
/// that is not a number) starts a new row, so a break in the numbering stays
/// visible instead of being closed up.

fn numbering<'a>(seats: impl Iterator<Item = &'a Seat>) -> Vec<(usize, usize)> {

    let mut positions = Vec::new();

    let mut row = 0;

    let mut col = 0;

    let mut previous: Option<u32> = None;

    for (index, seat) in seats.enumerate() {

        let value = seat.no.trim().parse::<u32>().ok();

        let continues = matches!((previous, value), (Some(p), Some(v)) if v == p + 1);

        if index > 0 && (!continues || col >= NUMBERING_ROW_WIDTH) {

            row += 1;

            col = 0;
        }

        positions.push((row, col));

        col += 1;

        previous = value;
    }

    positions
}

#[cfg(test)]

mod tests {

    use super::{Direction, Layout, NUMBERING_ROW_WIDTH, SeatGrid};
    use crate::libbook::Seat;

    fn numbered(values: &[&str]) -> Vec<Seat> {

        values
            .iter()
            .map(|value| {

                Seat {
                    no: value.to_string(),
                    ..Seat::default()
                }
            })
            .collect()
    }

    fn placed(points: &[(f64, f64)]) -> Vec<Seat> {

        points
            .iter()
            .enumerate()
            .map(|(index, &(x, y))| {

                Seat {
                    id: index.to_string(),
                    no: format!("{:03}", index + 1),
                    x: Some(x),
                    y: Some(y),
                    ..Seat::default()
                }
            })
            .collect()
    }

    #[test]

    fn coordinates_keep_the_room_shape() {

        // Two tables of 2x2 seats, side by side with an aisle between them.
        let seats = placed(&[
            (10.0, 10.0),
            (12.0, 10.0),
            (10.0, 14.0),
            (12.0, 14.0),
            (30.0, 10.0),
            (32.0, 10.0),
            (30.0, 14.0),
            (32.0, 14.0),
        ]);

        let grid = SeatGrid::build(&seats);

        assert_eq!(grid.layout, Layout::FloorPlan);

        assert_eq!(grid.position(0), Some((0, 0)));

        assert_eq!(grid.position(1), Some((0, 1)));

        assert_eq!(grid.position(2), Some((1, 0)));

        // The aisle stays one empty column wide instead of collapsing.
        assert_eq!(grid.position(4), Some((0, 3)));

        assert_eq!(grid.at(0, 2), None);

        assert_eq!((grid.rows, grid.cols), (2, 5));
    }

    #[test]

    fn small_jitter_shares_a_line() {

        // Plan coordinates are hand-placed; 10.0 and 10.2 are the same row.
        let seats = placed(&[(10.0, 10.0), (12.0, 10.2), (14.0, 9.9)]);

        let grid = SeatGrid::build(&seats);

        assert_eq!(grid.rows, 1);

        assert_eq!(grid.cols, 3);
    }

    #[test]

    fn colliding_seats_are_both_selectable() {

        let seats = placed(&[(10.0, 10.0), (10.0, 10.0), (14.0, 10.0)]);

        let grid = SeatGrid::build(&seats);

        let mut cells: Vec<_> = (0..seats.len()).filter_map(|i| grid.position(i)).collect();

        cells.sort();

        cells.dedup();

        assert_eq!(cells.len(), seats.len(), "每个座位都应有自己的格子");
    }

    #[test]

    fn missing_coordinates_fall_back_to_numbering() {

        let mut seats = placed(&[(10.0, 10.0), (12.0, 10.0)]);

        seats[1].x = None;

        assert_eq!(SeatGrid::build(&seats).layout, Layout::Numbering);
    }

    #[test]

    fn numbering_fills_rows_and_breaks_on_a_jump() {

        let values: Vec<String> = (1..=12).map(|value| format!("{value:03}")).collect();

        let mut labels: Vec<&str> = values.iter().map(String::as_str).collect();

        labels.push("292");

        let grid = SeatGrid::build(&numbered(&labels));

        assert_eq!(grid.position(0), Some((0, 0)));

        assert_eq!(grid.position(9), Some((0, 9)));

        assert_eq!(grid.position(10), Some((1, 0)));

        // 292 does not continue 012, so it starts its own row.
        assert_eq!(grid.position(12), Some((2, 0)));

        assert!(grid.cols <= NUMBERING_ROW_WIDTH);
    }

    #[test]

    fn steps_follow_the_map() {

        // 0 1 . 2
        // 3 . . 4
        let seats = placed(&[
            (10.0, 10.0),
            (12.0, 10.0),
            (30.0, 10.0),
            (10.0, 14.0),
            (30.0, 14.0),
        ]);

        let grid = SeatGrid::build(&seats);

        assert_eq!(grid.step(0, Direction::Right), Some(1));

        // Across the aisle, staying on the row.
        assert_eq!(grid.step(1, Direction::Right), Some(2));

        assert_eq!(grid.step(0, Direction::Down), Some(3));

        // Straight down from 2 is 4, not the nearer-by-index 3.
        assert_eq!(grid.step(2, Direction::Down), Some(4));

        assert_eq!(grid.step(0, Direction::Up), None);

        assert_eq!(grid.step(0, Direction::Left), None);
    }

    /// Seats no key can reach from some starting seat.
    ///
    /// How:
    /// Every seat is reachable from every other exactly when all of them are
    /// reachable from seat 0 and seat 0 is reachable from all of them, so one
    /// search along the moves and one against them is enough.

    fn stranded(grid: &SeatGrid, count: usize) -> Vec<usize> {

        let directions = [
            Direction::Up,
            Direction::Down,
            Direction::Left,
            Direction::Right,
        ];

        let edges: Vec<(usize, usize)> = (0..count)
            .flat_map(|from| {

                directions
                    .iter()
                    .filter_map(move |&direction| grid.step(from, direction).map(|to| (from, to)))
            })
            .collect();

        let search = |forward: bool| {

            let mut seen = vec![false; count];

            let mut stack = vec![0];

            seen[0] = true;

            while let Some(at) = stack.pop() {

                for &(from, to) in &edges {

                    let (here, next) = if forward { (from, to) } else { (to, from) };

                    if here == at && !seen[next] {

                        seen[next] = true;

                        stack.push(next);
                    }
                }
            }

            seen
        };

        let (reached, returns) = (search(true), search(false));

        (0..count)
            .filter(|&index| !reached[index] || !returns[index])
            .collect()
    }

    fn live(points: &[Option<(f64, f64)>]) -> Vec<Seat> {

        points
            .iter()
            .enumerate()
            .map(|(index, point)| {

                Seat {
                    id: index.to_string(),
                    no: format!("{:03}", index + 1),
                    x: point.map(|(x, _)| x),
                    y: point.map(|(_, y)| y),
                    ..Seat::default()
                }
            })
            .collect()
    }

    #[test]

    fn no_seat_is_stranded_on_a_sparse_map() {

        // Found by randomized search. Seat 4 sits in the middle, and each of
        // its neighbours had a cheaper seat ahead in every direction, so no
        // key ever moved onto it. Plus two seats on one spot.
        let seats = placed(&[
            (23.217, 28.464),
            (10.104, 1.566),
            (9.378, 29.256),
            (2.319, 8.865),
            (15.807, 18.642),
            (4.65, 22.107),
            (25.266, 3.711),
            (25.266, 3.711),
        ]);

        let grid = SeatGrid::build(&seats);

        assert_eq!(stranded(&grid, seats.len()), Vec::<usize>::new());
    }

    #[test]

    fn every_seat_is_reachable_on_the_live_floor_plans() {

        for (name, points, unplaced) in [("area 8", &AREA_8[..], 0), ("area 16", &AREA_16[..], 1)] {

            let seats = live(points);

            let grid = SeatGrid::build(&seats);

            // Area 16 has one seat without a position; the other 194 still
            // have a plan, and it is drawn.
            assert_eq!(grid.layout, Layout::FloorPlan, "{name}");

            assert_eq!(grid.reading_order().count(), seats.len(), "{name}");

            assert_eq!(grid.unplaced, unplaced, "{name}");

            assert_eq!(
                stranded(&grid, seats.len()),
                Vec::<usize>::new(),
                "{name}: 有座位无法用方向键选中"
            );
        }
    }

    #[test]

    fn seats_without_coordinates_are_listed_below_the_plan() {

        let mut seats = placed(&[
            (10.0, 10.0),
            (12.0, 10.0),
            (10.0, 14.0),
            (12.0, 14.0),
            (30.0, 10.0),
        ]);

        seats[4].x = None;

        let grid = SeatGrid::build(&seats);

        assert_eq!(grid.layout, Layout::FloorPlan);

        // The seats that have a position keep it.
        assert_eq!(grid.position(0), Some((0, 0)));

        assert_eq!(grid.position(3), Some((1, 1)));

        // The one without is set apart after an empty row, not mixed in.
        assert_eq!(grid.position(4), Some((3, 0)));

        assert!((0..grid.cols).all(|col| grid.at(2, col).is_none()));

        assert_eq!(grid.unplaced, 1);

        assert!(
            grid.caption().contains("部分座位无坐标"),
            "{}",
            grid.caption()
        );

        assert_eq!(stranded(&grid, seats.len()), Vec::<usize>::new());
    }

    /// `(point_x, point_y)` of every seat in `seat-map --area 8 --json`,
    /// in service order.
    ///
    /// A data table; kept compact rather than one entry per line.

    #[rustfmt::skip]

    const AREA_8: [Option<(f64, f64)>; 175] = [
        Some((15.98958, 12.8876)), Some((15.98958, 15.89147)), Some((15.98958, 18.89535)), Some((15.9375, 21.80233)),
        Some((15.98958, 24.5155)), Some((15.9375, 27.61628)), Some((15.88542, 30.62016)), Some((15.98958, 33.43023)),
        Some((15.88542, 36.62791)), Some((16.04167, 39.24419)), Some((15.98958, 42.15116)), Some((15.9375, 45.05814)),
        Some((16.04167, 47.96511)), Some((16.04167, 50.96899)), Some((15.98958, 53.77907)), Some((15.98958, 56.87984)),
        Some((16.04167, 59.78682)), Some((15.9375, 62.5)), Some((15.98958, 65.50388)), Some((15.98958, 68.41085)),
        Some((15.98958, 71.51163)), Some((16.04167, 74.4186)), Some((16.04167, 77.32558)), Some((15.98958, 80.32946)),
        Some((15.98958, 83.13953)), Some((15.98958, 86.14341)), Some((23.75, 84.68993)), Some((25.36458, 84.39922)),
        Some((27.13542, 84.39922)), Some((28.80208, 84.59303)), Some((23.07292, 28.10077)), Some((24.89583, 28.00388)),
        Some((26.35417, 28.10077)), Some((23.07292, 31.68605)), Some((24.84375, 31.49225)), Some((26.45833, 31.78295)),
        Some((32.29167, 28.10077)), Some((34.11458, 28.10077)), Some((35.78125, 28.19767)), Some((37.44792, 28.29457)),
        Some((32.39583, 31.68605)), Some((34.11458, 31.58915)), Some((35.78125, 31.68605)), Some((37.34375, 31.49225)),
        Some((23.125, 46.60853)), Some((24.79167, 46.51163)), Some((26.40625, 46.51163)), Some((23.07292, 50.1938)),
        Some((24.84375, 50.1938)), Some((26.35417, 50.0)), Some((32.34375, 46.70543)), Some((34.0625, 46.60853)),
        Some((35.72917, 46.70543)), Some((37.34375, 46.60853)), Some((32.39583, 50.2907)), Some((34.0625, 50.0969)),
        Some((35.625, 50.0)), Some((37.39583, 50.2907)), Some((23.22917, 65.21318)), Some((24.73958, 65.11628)),
        Some((26.51042, 65.21318)), Some((23.17708, 68.60465)), Some((24.79167, 68.50775)), Some((26.5625, 68.41085)),
        Some((32.39583, 65.01938)), Some((34.01042, 65.01938)), Some((35.625, 64.82558)), Some((37.34375, 65.01938)),
        Some((32.34375, 68.79845)), Some((34.01042, 68.50775)), Some((35.67708, 68.70155)), Some((37.34375, 68.60465)),
        Some((54.58333, 40.11628)), Some((56.09375, 40.21318)), Some((57.8125, 39.92248)), Some((59.47917, 40.21318)),
        Some((54.53125, 43.79845)), Some((56.09375, 43.60465)), Some((54.47917, 49.03101)), Some((56.19792, 49.03101)),
        Some((57.76042, 49.03101)), Some((59.47917, 49.12791)), Some((54.63542, 52.51938)), Some((56.14583, 52.42248)),
        Some((57.91667, 52.51938)), Some((59.47917, 52.71318)), Some((54.53125, 67.92635)), Some((56.25, 67.92635)),
        Some((57.8125, 68.02325)), Some((59.53125, 68.02325)), Some((54.58333, 71.60853)), Some((56.25, 71.51163)),
        Some((57.86458, 71.51163)), Some((59.47917, 71.41473)), Some((54.53125, 76.25969)), Some((56.19792, 76.35659)),
        Some((54.58333, 80.03876)), Some((56.25, 79.94186)), Some((57.86458, 80.03876)), Some((59.53125, 79.74806)),
        Some((70.88541, 21.70543)), Some((72.65625, 21.70543)), Some((74.21875, 21.51163)), Some((76.04166, 21.70543)),
        Some((77.65625, 21.60853)), Some((79.27084, 21.89923)), Some((70.9375, 30.91085)), Some((72.65625, 30.91085)),
        Some((74.375, 31.10465)), Some((76.04166, 31.00775)), Some((77.55209, 31.00775)), Some((79.27084, 31.00775)),
        Some((71.04166, 34.59302)), Some((72.70834, 34.39922)), Some((74.375, 34.30233)), Some((75.88541, 34.30233)),
        Some((77.60416, 34.68992)), Some((79.27084, 34.59302)), Some((70.98959, 47.57752)), Some((72.70834, 47.67442)),
        Some((74.32291, 47.57752)), Some((75.98959, 47.48062)), Some((77.65625, 47.48062)), Some((79.27084, 47.57752)),
        Some((71.04166, 51.16279)), Some((72.70834, 51.25969)), Some((74.27084, 51.16279)), Some((75.98959, 51.35659)),
        Some((77.65625, 51.16279)), Some((79.32291, 51.35659)), Some((71.04166, 57.75194)), Some((72.65625, 57.94574)),
        Some((74.32291, 57.94574)), Some((75.98959, 57.75194)), Some((77.60416, 57.75194)), Some((71.09375, 61.33721)),
        Some((72.70834, 61.04651)), Some((74.375, 61.14341)), Some((75.9375, 61.33721)), Some((77.60416, 61.33721)),
        Some((71.04166, 67.92635)), Some((72.70834, 67.82946)), Some((74.32291, 67.92635)), Some((75.98959, 67.92635)),
        Some((77.60416, 68.02325)), Some((79.375, 67.92635)), Some((71.09375, 71.60853)), Some((72.65625, 71.51163)),
        Some((74.32291, 71.51163)), Some((75.9375, 71.60853)), Some((77.60416, 71.70543)), Some((79.21875, 71.51163)),
        Some((70.98959, 83.62403)), Some((72.60416, 83.81783)), Some((74.32291, 83.72093)), Some((75.98959, 83.62403)),
        Some((77.60416, 83.72093)), Some((79.375, 83.91473)), Some((70.98959, 87.3062)), Some((72.8125, 87.3062)),
        Some((74.32291, 87.2093)), Some((75.9375, 87.1124)), Some((77.70834, 87.2093)), Some((79.27084, 87.0155)),
        Some((61.04167, 93.12016)), Some((62.76042, 92.92635)), Some((64.42709, 89.34109)), Some((65.98959, 89.14729)),
        Some((67.60416, 92.92635)), Some((69.27084, 92.92635)), Some((71.04166, 92.82946)), Some((72.65625, 92.82946)),
        Some((74.375, 92.92635)), Some((76.04166, 92.92635)), Some((77.55209, 92.92635)),
    ];

    /// `(point_x, point_y)` of every seat in `seat-map --area 16 --json`,
    /// in service order, with seat 119 returned without a position.
    ///
    /// A data table; kept compact rather than one entry per line.

    #[rustfmt::skip]

    const AREA_16: [Option<(f64, f64)>; 195] = [
        Some((38.17708, 91.95737)), Some((39.79167, 92.05426)), Some((41.45833, 91.95737)), Some((43.02083, 91.95737)),
        Some((38.125, 95.54263)), Some((39.73958, 95.54263)), Some((41.51042, 95.63953)), Some((43.07292, 95.63953)),
        Some((13.33333, 76.45349)), Some((13.38542, 72.86822)), Some((13.33333, 69.18604)), Some((13.33333, 65.01938)),
        Some((13.38542, 61.33721)), Some((13.38542, 57.55814)), Some((13.38542, 53.77907)), Some((13.4375, 49.9031)),
        Some((13.33333, 46.12403)), Some((13.38542, 42.34496)), Some((13.33333, 38.46899)), Some((13.38542, 34.68992)),
        Some((13.38542, 30.91085)), Some((13.4375, 26.93798)), Some((13.33333, 23.15891)), Some((13.38542, 19.37984)),
        Some((24.27083, 28.19767)), Some((25.9375, 28.00388)), Some((24.21875, 31.68605)), Some((25.9375, 31.68605)),
        Some((24.27083, 35.56202)), Some((25.88542, 35.46511)), Some((24.16667, 39.05039)), Some((25.9375, 39.14729)),
        Some((24.21875, 43.12016)), Some((25.88542, 43.02325)), Some((24.21875, 46.51163)), Some((25.98958, 46.60853)),
        Some((24.21875, 50.4845)), Some((25.9375, 50.2907)), Some((24.27083, 53.97287)), Some((25.83333, 53.97287)),
        Some((24.27083, 57.94574)), Some((25.9375, 57.84884)), Some((24.21875, 61.33721)), Some((25.88542, 61.43411)),
        Some((24.21875, 65.31007)), Some((25.83333, 65.31007)), Some((24.32292, 68.89535)), Some((25.88542, 68.79845)),
        Some((24.21875, 72.77132)), Some((25.88542, 72.86822)), Some((24.21875, 76.0659)), Some((25.98958, 76.0659)),
        Some((24.16667, 80.03876)), Some((25.9375, 80.03876)), Some((24.21875, 83.43023)), Some((25.9375, 83.43023)),
        Some((39.73958, 13.27519)), Some((41.40625, 13.27519)), Some((42.96875, 13.37209)), Some((44.58333, 13.37209)),
        Some((39.6875, 16.95737)), Some((41.35417, 16.86047)), Some((43.07292, 16.86047)), Some((44.58333, 16.95737)),
        Some((39.6875, 19.86434)), Some((41.40625, 19.86434)), Some((43.02083, 19.86434)), Some((44.63542, 19.96124)),
        Some((39.73958, 23.44961)), Some((41.40625, 23.44961)), Some((43.02083, 23.35271)), Some((44.63542, 23.15891)),
        Some((56.35417, 13.27519)), Some((57.96875, 13.27519)), Some((59.6875, 13.17829)), Some((61.30208, 13.17829)),
        Some((56.35417, 16.76357)), Some((58.02083, 16.86047)), Some((59.73958, 16.95737)), Some((61.30208, 16.95737)),
        Some((56.25, 19.86434)), Some((58.02083, 19.67054)), Some((59.58333, 19.67054)), Some((61.25, 19.76744)),
        Some((56.30208, 23.35271)), Some((58.07292, 23.35271)), Some((59.63542, 23.44961)), Some((61.35417, 23.35271)),
        Some((67.8125, 15.21318)), Some((69.47916, 15.01938)), Some((71.14584, 15.01938)), Some((72.76041, 15.21318)),
        Some((67.8125, 18.79845)), Some((69.47916, 18.89535)), Some((71.19791, 18.89535)), Some((72.70834, 18.79845)),
        Some((67.76041, 21.80233)), Some((69.47916, 21.80233)), Some((71.14584, 21.70543)), Some((72.76041, 21.80233)),
        Some((67.86459, 25.4845)), Some((69.47916, 25.4845)), Some((71.14584, 25.2907)), Some((72.86459, 25.3876)),
        Some((67.86459, 28.10077)), Some((69.47916, 28.10077)), Some((71.14584, 28.19767)), Some((72.76041, 28.19767)),
        Some((67.86459, 31.78295)), Some((69.47916, 31.78295)), Some((71.14584, 31.78295)), Some((72.70834, 31.78295)),
        Some((67.8125, 35.07752)), Some((69.47916, 35.17442)), Some((71.19791, 35.07752)), Some((71.04166, 38.66279)),
        Some((67.8125, 38.75969)), Some((69.53125, 38.66279)), None, Some((72.70834, 38.75969)),
        Some((67.8125, 43.12016)), Some((69.42709, 42.92636)), Some((71.14584, 42.73256)), Some((72.70834, 42.73256)),
        Some((67.8125, 46.51163)), Some((69.53125, 46.31783)), Some((71.14584, 46.60853)), Some((72.76041, 46.31783)),
        Some((67.8125, 50.87209)), Some((69.42709, 50.77519)), Some((71.14584, 50.87209)), Some((72.70834, 50.77519)),
        Some((67.8125, 54.36047)), Some((69.47916, 54.55426)), Some((71.14584, 54.55426)), Some((72.76041, 54.45736)),
        Some((67.8125, 57.26744)), Some((69.53125, 57.36434)), Some((71.04166, 57.36434)), Some((72.70834, 57.55814)),
        Some((67.8125, 60.85271)), Some((69.47916, 60.85271)), Some((71.09375, 60.75581)), Some((72.8125, 60.85271)),
        Some((67.86459, 63.95349)), Some((69.375, 63.85659)), Some((71.14584, 63.85659)), Some((72.76041, 63.66279)),
        Some((67.86459, 67.24806)), Some((69.47916, 67.15116)), Some((71.19791, 67.34496)), Some((72.76041, 67.34496)),
        Some((67.76041, 71.70543)), Some((69.47916, 71.51163)), Some((71.19791, 71.60853)), Some((72.76041, 71.60853)),
        Some((67.8125, 75.0969)), Some((69.53125, 75.0)), Some((71.19791, 75.3876)), Some((72.8125, 75.2907)),
        Some((67.8125, 78.19768)), Some((69.42709, 78.29457)), Some((70.98959, 78.00388)), Some((72.86459, 78.10078)),
        Some((67.91666, 81.78294)), Some((69.47916, 81.58915)), Some((71.09375, 81.68604)), Some((72.8125, 81.78294)),
        Some((77.13541, 78.10078)), Some((78.75, 78.10078)), Some((80.46875, 78.10078)), Some((82.13541, 78.00388)),
        Some((77.1875, 81.78294)), Some((78.90625, 81.68604)), Some((80.52084, 81.87984)), Some((82.13541, 81.87984)),
        Some((67.86459, 84.59303)), Some((69.47916, 84.78682)), Some((71.09375, 84.68993)), Some((72.76041, 84.78682)),
        Some((67.8125, 88.27519)), Some((69.58334, 88.0814)), Some((71.19791, 88.27519)), Some((72.76041, 88.27519)),
        Some((77.1875, 84.68993)), Some((78.85416, 84.78682)), Some((80.46875, 84.59303)), Some((82.1875, 84.59303)),
        Some((77.13541, 88.37209)), Some((78.80209, 88.46899)), Some((80.57291, 88.46899)), Some((82.13541, 88.17829)),
        Some((58.02083, 92.15116)), Some((59.63542, 92.15116)), Some((61.25, 91.95737)),
    ];

    #[test]

    fn an_empty_list_produces_an_empty_grid() {

        let grid = SeatGrid::build(&[]);

        assert_eq!((grid.rows, grid.cols), (0, 0));

        assert_eq!(grid.at(0, 0), None);

        assert_eq!(grid.step(0, Direction::Down), None);
    }
}
