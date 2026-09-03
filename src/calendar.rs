//! Small local clock and month-calendar helpers.

use chrono::{Datelike, Duration, Local, NaiveDate};

/// One cell in a six-week Monday-first calendar grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarCell {
    pub day: u32,
    pub in_current_month: bool,
    pub is_today: bool,
}

/// First day of the month containing `date`.
pub fn month_start(date: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(date.year(), date.month(), 1).expect("valid calendar month")
}

/// Shift a first-of-month date by `offset` months.
pub fn shift_month(month: NaiveDate, offset: i32) -> NaiveDate {
    let month_index = month.year() * 12 + month.month0() as i32 + offset;
    let year = month_index.div_euclid(12);
    let month = month_index.rem_euclid(12) as u32 + 1;
    NaiveDate::from_ymd_opt(year, month, 1).expect("valid shifted calendar month")
}

/// Build a fixed six-row, Monday-first month grid.
pub fn month_grid(month: NaiveDate, today: NaiveDate) -> Vec<CalendarCell> {
    let month = month_start(month);
    let grid_start = month - Duration::days(month.weekday().num_days_from_monday() as i64);

    (0..42)
        .map(|offset| {
            let date = grid_start + Duration::days(offset);
            CalendarCell {
                day: date.day(),
                in_current_month: date.month() == month.month() && date.year() == month.year(),
                is_today: date == today,
            }
        })
        .collect()
}

/// Format the local clock as a concise launcher display.
pub fn local_clock_text() -> (String, String) {
    let now = Local::now();
    (
        now.format("%H:%M").to_string(),
        now.format("%A, %-d %B %Y").to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_grid_contains_six_weeks() {
        let month = NaiveDate::from_ymd_opt(2026, 9, 1).expect("valid date");
        let grid = month_grid(month, month);

        assert_eq!(grid.len(), 42);
        assert!(grid.iter().any(|cell| cell.is_today));
        assert_eq!(grid.iter().filter(|cell| cell.in_current_month).count(), 30);
    }

    #[test]
    fn shifting_a_month_wraps_year_boundaries() {
        let december = NaiveDate::from_ymd_opt(2026, 12, 1).expect("valid date");
        assert_eq!(
            shift_month(december, 1),
            NaiveDate::from_ymd_opt(2027, 1, 1).expect("valid date")
        );
    }
}
