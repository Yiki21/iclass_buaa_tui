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
//! visible without stretching the map. When any seat lacks a position the area
//! has no plan, and seats are laid out by their numbering instead.

use std::collections::HashSet;

use super::Seat;

/// Which source the layout was built from.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]

pub enum Layout {
    /// Built from the floor-plan coordinates the service returned.
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
    pub layout: Layout,
    pub rows:   usize,
    pub cols:   usize,
    /// `(row, col)` of each seat, aligned with the seat list it was built from.
    positions:  Vec<(usize, usize)>,
    /// Seat index at each cell, row-major.
    cells:      Vec<Option<usize>>,
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
    /// Lays out `seats`, from their floor-plan coordinates when every seat has
    /// them and from their numbering otherwise.

    pub fn build(seats: &[Seat]) -> Self {

        let points: Option<Vec<(f64, f64)>> =
            seats.iter().map(|seat| Some((seat.x?, seat.y?))).collect();

        match points {
            Some(points) if !points.is_empty() => {
                Self::from_positions(Layout::FloorPlan, floor_plan(&points))
            }
            _ => Self::from_positions(Layout::Numbering, numbering(seats)),
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
    /// Among seats strictly ahead, the nearest wins, where sideways drift
    /// counts double. Moving down a column therefore prefers the seat directly
    /// below two rows away over a diagonal neighbour one row away, which is
    /// what the eye expects on a map with aisles.

    pub fn step(&self, from: usize, direction: Direction) -> Option<usize> {

        let (row, col) = self.position(from)?;

        self.positions
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != from)
            .filter_map(|(index, &(other_row, other_col))| {

                let (ahead, lateral) = match direction {
                    Direction::Up => (row as isize - other_row as isize, col.abs_diff(other_col)),
                    Direction::Down => (other_row as isize - row as isize, col.abs_diff(other_col)),
                    Direction::Left => (col as isize - other_col as isize, row.abs_diff(other_row)),
                    Direction::Right => {
                        (other_col as isize - col as isize, row.abs_diff(other_row))
                    }
                };

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
            positions,
            cells,
        }
    }
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

fn numbering(seats: &[Seat]) -> Vec<(usize, usize)> {

    let mut positions = Vec::with_capacity(seats.len());

    let mut row = 0;

    let mut col = 0;

    let mut previous: Option<u32> = None;

    for (index, seat) in seats.iter().enumerate() {

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

    #[test]

    fn an_empty_list_produces_an_empty_grid() {

        let grid = SeatGrid::build(&[]);

        assert_eq!((grid.rows, grid.cols), (0, 0));

        assert_eq!(grid.at(0, 0), None);

        assert_eq!(grid.step(0, Direction::Down), None);
    }
}
