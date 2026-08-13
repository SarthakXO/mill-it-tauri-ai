use super::types::DateRange;
use chrono::{Datelike, Duration, Months, NaiveDate, NaiveDateTime};

pub(crate) fn latest_complete_business_date(data_as_of: &str) -> Option<NaiveDate> {
    let offset_index = data_as_of
        .char_indices()
        .skip(10)
        .find_map(|(index, character)| matches!(character, '+' | '-').then_some(index))?;
    let local = data_as_of.get(..offset_index)?.trim().replace('T', " ");
    let offset = data_as_of.get(offset_index..)?;
    let timestamp = NaiveDateTime::parse_from_str(&local, "%Y-%m-%d %H:%M:%S%.f").ok()?;
    let sign = if offset.starts_with('-') { -1 } else { 1 };
    let clean = offset.trim_start_matches(['+', '-']);
    let (hours, minutes) = clean
        .split_once(':')
        .map(|(hours, minutes)| (hours.parse::<i64>().ok(), minutes.parse::<i64>().ok()))
        .unwrap_or_else(|| (clean.parse::<i64>().ok(), Some(0)));
    let utc = timestamp - Duration::minutes(sign * (hours? * 60 + minutes?));
    let ist = utc + Duration::minutes(330);
    Some(ist.date() - Duration::days(1))
}

pub(crate) fn complete_days_range(latest_complete: NaiveDate, days: i64) -> DateRange {
    let end_exclusive = latest_complete + Duration::days(1);
    DateRange {
        start: end_exclusive - Duration::days(days.max(1)),
        end_exclusive,
    }
}

pub(crate) fn month_range(year: i32, month: u32) -> Option<DateRange> {
    let start = NaiveDate::from_ymd_opt(year, month, 1)?;
    let end_exclusive = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)?
    };
    Some(DateRange {
        start,
        end_exclusive,
    })
}

pub(crate) fn previous_complete_month(latest_complete: NaiveDate) -> Option<DateRange> {
    let current_month =
        NaiveDate::from_ymd_opt(latest_complete.year(), latest_complete.month(), 1)?;
    let previous_day = current_month - Duration::days(1);
    month_range(previous_day.year(), previous_day.month())
}

pub(crate) fn subtract_months(date: NaiveDate, months: u32) -> Option<NaiveDate> {
    date.checked_sub_months(Months::new(months))
}

pub(crate) fn iso(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_the_freshest_partial_ist_day() {
        assert_eq!(
            latest_complete_business_date("2026-08-11 06:16:44.439091+00"),
            NaiveDate::from_ymd_opt(2026, 8, 10)
        );
    }
}
