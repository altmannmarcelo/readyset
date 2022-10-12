use std::borrow::Borrow;
use std::cmp::Ordering;
use std::ops::{Add, Div, Mul, Sub};
use std::str::FromStr;

use chrono::{Datelike, LocalResult, NaiveDate, NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use launchpad::redacted::Sensitive;
use maths::int::integer_rnd;
use mysql_time::MySqlTime;
use readyset_data::{Collation, DfType, DfValue};
use readyset_errors::{ReadySetError, ReadySetResult};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use vec1::Vec1;

use crate::{BuiltinFunction, Expr};

macro_rules! try_cast_or_none {
    ($df_value:expr, $to_ty:expr, $from_ty:expr) => {{
        match $df_value.coerce_to($to_ty, $from_ty) {
            Ok(v) => v,
            Err(_) => return Ok(DfValue::None),
        }
    }};
}

macro_rules! non_null_owned {
    ($df_value:expr) => {{
        let val = $df_value;
        if val.is_none() {
            return Ok(DfValue::None);
        } else {
            val
        }
    }};
}

/// Returns the type of data stored in a JSON value as a string.
fn get_json_value_type(json: &serde_json::Value) -> &'static str {
    match json {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Attempts to coerce the value to `Timestamp` or `Time`, otherwise defaults to null on failure.
fn get_time_or_default(value: &DfValue, from_ty: &DfType) -> DfValue {
    // Default to 0 for consistency rather than rely on type dialect.
    // TODO: Use the database's real default.
    let subsecond_digits = from_ty.subsecond_digits().unwrap_or_default();

    value
        .coerce_to(&DfType::Timestamp { subsecond_digits }, from_ty)
        .or_else(|_| value.coerce_to(&DfType::Time { subsecond_digits }, from_ty))
        .unwrap_or(DfValue::None)
}

/// Transforms a `[NaiveDateTime]` into a new one with a different timezone.
/// The `[NaiveDateTime]` is interpreted as having the timezone specified by the
/// `src` parameter, and then it's transformed to timezone specified by the `target` parameter.
fn convert_tz(datetime: &NaiveDateTime, src: &str, target: &str) -> ReadySetResult<NaiveDateTime> {
    let mk_err = |message: &str| ReadySetError::ProjectExprBuiltInFunctionError {
        function: "convert_tz".to_owned(),
        message: message.to_owned(),
    };

    let src_tz: Tz = src
        .parse()
        .map_err(|_| mk_err("Failed to parse the source timezone"))?;
    let target_tz: Tz = target
        .parse()
        .map_err(|_| mk_err("Failed to parse the target timezone"))?;

    let datetime_tz = match src_tz.from_local_datetime(datetime) {
        LocalResult::Single(dt) => dt,
        LocalResult::None => {
            return Err(mk_err(
                "Failed to transform the datetime to a different timezone",
            ))
        }
        LocalResult::Ambiguous(_, _) => {
            return Err(mk_err(
                "Failed to transform the datetime to a different timezone",
            ))
        }
    };

    Ok(datetime_tz.with_timezone(&target_tz).naive_local())
}

fn day_of_week(date: &NaiveDate) -> u8 {
    date.weekday().number_from_sunday() as u8
}

fn month(date: &NaiveDate) -> u8 {
    date.month() as u8
}

fn timediff_datetimes(time1: &NaiveDateTime, time2: &NaiveDateTime) -> MySqlTime {
    let duration = time1.sub(*time2);
    MySqlTime::new(duration)
}

fn timediff_times(time1: &MySqlTime, time2: &MySqlTime) -> MySqlTime {
    time1.sub(*time2)
}

fn addtime_datetime(time1: &NaiveDateTime, time2: &MySqlTime) -> NaiveDateTime {
    time2.add(*time1)
}

fn addtime_times(time1: &MySqlTime, time2: &MySqlTime) -> MySqlTime {
    time1.add(*time2)
}

fn greatest_or_least<F, D>(
    args: &Vec1<Expr>,
    record: &[D],
    compare_as: &DfType,
    ty: &DfType,
    mut compare: F,
) -> ReadySetResult<DfValue>
where
    F: FnMut(&DfValue, &DfValue) -> bool,
    D: Borrow<DfValue>,
{
    let arg1 = args.first();
    let mut res = non_null_owned!(arg1.eval(record)?);
    let mut res_ty = arg1.ty();
    let mut res_compare = try_cast_or_none!(res, compare_as, arg1.ty());
    for arg in args.iter().skip(1) {
        let val = non_null_owned!(arg.eval(record)?);
        let val_compare = try_cast_or_none!(val, compare_as, arg.ty());
        if compare(&val_compare, &res_compare) {
            res = val;
            res_ty = arg.ty();
            res_compare = val_compare;
        }
    }
    Ok(try_cast_or_none!(res, ty, res_ty))
}

impl BuiltinFunction {
    pub(crate) fn eval<D>(&self, ty: &DfType, record: &[D]) -> ReadySetResult<DfValue>
    where
        D: Borrow<DfValue>,
    {
        match self {
            BuiltinFunction::ConvertTZ {
                args: [arg1, arg2, arg3],
                subsecond_digits,
            } => {
                let param1 = arg1.eval(record)?;
                let param2 = arg2.eval(record)?;
                let param3 = arg3.eval(record)?;

                let param1_cast = try_cast_or_none!(
                    param1,
                    &DfType::Timestamp {
                        subsecond_digits: *subsecond_digits
                    },
                    arg1.ty()
                );
                let param2_cast =
                    try_cast_or_none!(param2, &DfType::Text(Collation::default()), arg2.ty());
                let param3_cast =
                    try_cast_or_none!(param3, &DfType::Text(Collation::default()), arg3.ty());

                match convert_tz(
                    &(NaiveDateTime::try_from(&param1_cast))?,
                    <&str>::try_from(&param2_cast)?,
                    <&str>::try_from(&param3_cast)?,
                ) {
                    Ok(v) => Ok(DfValue::TimestampTz(v.into())),
                    Err(_) => Ok(DfValue::None),
                }
            }
            BuiltinFunction::DayOfWeek(arg) => {
                let param = arg.eval(record)?;
                let param_cast = try_cast_or_none!(param, &DfType::Date, arg.ty());
                Ok(DfValue::Int(
                    day_of_week(&(NaiveDate::try_from(&param_cast)?)) as i64,
                ))
            }
            BuiltinFunction::IfNull(arg1, arg2) => {
                let param1 = arg1.eval(record)?;
                let param2 = arg2.eval(record)?;
                if param1.is_none() {
                    Ok(param2)
                } else {
                    Ok(param1)
                }
            }
            BuiltinFunction::Month(arg) => {
                let param = arg.eval(record)?;
                let param_cast = try_cast_or_none!(param, &DfType::Date, arg.ty());
                Ok(DfValue::UnsignedInt(
                    month(&(NaiveDate::try_from(non_null!(param_cast))?)) as u64,
                ))
            }
            BuiltinFunction::Timediff(arg1, arg2) => {
                let param1 = arg1.eval(record)?;
                let param2 = arg2.eval(record)?;
                let null_result = Ok(DfValue::None);
                let time_param1 = get_time_or_default(&param1, arg1.ty());
                let time_param2 = get_time_or_default(&param2, arg2.ty());
                if time_param1.is_none()
                    || time_param1
                        .sql_type()
                        .and_then(|st| time_param2.sql_type().map(|st2| (st, st2)))
                        .filter(|(st1, st2)| st1.eq(st2))
                        .is_none()
                {
                    return null_result;
                }
                let time = if time_param1.is_datetime() {
                    timediff_datetimes(
                        &(NaiveDateTime::try_from(&time_param1)?),
                        &(NaiveDateTime::try_from(&time_param2)?),
                    )
                } else {
                    timediff_times(
                        &(MySqlTime::try_from(&time_param1)?),
                        &(MySqlTime::try_from(&time_param2)?),
                    )
                };
                Ok(DfValue::Time(time))
            }
            BuiltinFunction::Addtime(arg1, arg2) => {
                let param1 = arg1.eval(record)?;
                let param2 = arg2.eval(record)?;
                let time_param2 = get_time_or_default(&param2, arg2.ty());
                if time_param2.is_datetime() {
                    return Ok(DfValue::None);
                }
                let time_param1 = get_time_or_default(&param1, arg1.ty());
                if time_param1.is_datetime() {
                    Ok(DfValue::TimestampTz(
                        addtime_datetime(
                            &(NaiveDateTime::try_from(&time_param1)?),
                            &(MySqlTime::try_from(&time_param2)?),
                        )
                        .into(),
                    ))
                } else {
                    Ok(DfValue::Time(addtime_times(
                        &(MySqlTime::try_from(&time_param1)?),
                        &(MySqlTime::try_from(&time_param2)?),
                    )))
                }
            }
            BuiltinFunction::Round(arg1, arg2) => {
                let expr = arg1.eval(record)?;
                let param2 = arg2.eval(record)?;
                let rnd_prec = match non_null!(param2) {
                    DfValue::Int(inner) => *inner as i32,
                    DfValue::UnsignedInt(inner) => *inner as i32,
                    DfValue::Float(f) => f.round() as i32,
                    DfValue::Double(f) => f.round() as i32,
                    DfValue::Numeric(ref d) => {
                        // TODO(fran): I don't know if this is the right thing to do.
                        d.round().to_i32().ok_or_else(|| {
                            ReadySetError::BadRequest(format!(
                                "NUMERIC value {} exceeds 32-byte integer size",
                                d
                            ))
                        })?
                    }
                    _ => 0,
                };

                macro_rules! round {
                    ($real:expr, $real_type:ty) => {{
                        let base: $real_type = 10.0;
                        if rnd_prec > 0 {
                            // If rounding precision is positive, than we keep the returned
                            // type as a float. We never return greater precision than was
                            // stored so we choose the minimum of stored precision or rounded
                            // precision.
                            let rounded_float = ($real * base.powf(rnd_prec as $real_type)).round()
                                / base.powf(rnd_prec as $real_type);
                            let real = DfValue::try_from(rounded_float).unwrap();
                            Ok(real)
                        } else {
                            // Rounding precision is negative, so we need to zero out some
                            // digits.
                            let rounded_float = (($real / base.powf(-rnd_prec as $real_type))
                                .round()
                                * base.powf(-rnd_prec as $real_type));
                            let real = DfValue::try_from(rounded_float).unwrap();
                            Ok(real)
                        }
                    }};
                }

                match non_null!(expr) {
                    DfValue::Float(float) => round!(float, f32),
                    DfValue::Double(double) => round!(double, f64),
                    DfValue::Int(val) => {
                        let rounded = integer_rnd(*val as i128, rnd_prec);
                        Ok(DfValue::Int(rounded as _))
                    }
                    DfValue::UnsignedInt(val) => {
                        let rounded = integer_rnd(*val as i128, rnd_prec);
                        Ok(DfValue::Int(rounded as _))
                    }
                    DfValue::Numeric(d) => {
                        let rounded_dec = if rnd_prec >= 0 {
                            d.round_dp_with_strategy(
                                rnd_prec as _,
                                rust_decimal::RoundingStrategy::MidpointAwayFromZero,
                            )
                        } else {
                            let factor = Decimal::from_f64(10.0f64.powf(-rnd_prec as _)).unwrap();

                            d.div(factor)
                                .round_dp_with_strategy(
                                    0,
                                    rust_decimal::RoundingStrategy::MidpointAwayFromZero,
                                )
                                .mul(factor)
                        };

                        Ok(DfValue::Numeric(rounded_dec.into()))
                    }
                    dt => {
                        let dt_str = dt.to_string();
                        // MySQL will parse as many characters as it possibly can from a string
                        // as double
                        let mut double = 0f64;
                        let mut chars = 1;
                        if dt_str.starts_with('-') {
                            chars += 1;
                        }
                        while chars < dt_str.len() {
                            // This is very sad that Rust doesn't tell us how many characters of
                            // a string it was able to parse, but for now we just try to parse
                            // incrementally more characters until we fail
                            match dt_str[..chars].parse() {
                                Ok(v) => {
                                    double = v;
                                    chars += 1;
                                }
                                Err(_) => break,
                            }
                        }
                        round!(double, f64)
                    }
                }
            }
            BuiltinFunction::JsonTypeof(expr) | BuiltinFunction::JsonbTypeof(expr) => {
                // TODO: Change this to coerce to `SqlType::Jsonb` and have it return a
                // `DfValue` actually representing JSON.
                let val = try_cast_or_none!(
                    non_null!(&expr.eval(record)?),
                    &DfType::Text(Collation::default()),
                    expr.ty()
                );
                let json_str = <&str>::try_from(&val)?;

                let json = serde_json::Value::from_str(json_str).map_err(|e| -> ReadySetError {
                    ReadySetError::ProjectExprBuiltInFunctionError {
                        function: self.name().into(),
                        message: format!("parsing JSON expression failed: {}", Sensitive(&e)),
                    }
                })?;

                Ok(get_json_value_type(&json).into())
            }
            BuiltinFunction::Coalesce(arg1, rest_args) => {
                let val1 = arg1.eval(record)?;
                let rest_vals = rest_args
                    .iter()
                    .map(|expr| expr.eval(record))
                    .collect::<Result<Vec<_>, _>>()?;
                if !val1.is_none() {
                    Ok(val1)
                } else {
                    Ok(rest_vals
                        .into_iter()
                        .find(|v| !v.is_none())
                        .unwrap_or(DfValue::None))
                }
            }
            BuiltinFunction::Concat(arg1, rest_args) => {
                let mut s =
                    <&str>::try_from(&non_null!(arg1.eval(record)?).coerce_to(ty, arg1.ty())?)?
                        .to_owned();

                for arg in rest_args {
                    let val = non_null!(arg.eval(record)?).coerce_to(ty, arg.ty())?;
                    s.push_str((&val).try_into()?)
                }

                Ok(s.into())
            }
            BuiltinFunction::Substring(string, from, len) => {
                let string = non_null!(string.eval(record)?).coerce_to(ty, string.ty())?;
                let s = <&str>::try_from(&string)?;

                let from = match from {
                    Some(from) => non_null!(from.eval(record)?)
                        .coerce_to(&DfType::BigInt, from.ty())?
                        .try_into()?,
                    None => 1i64,
                };

                let len = match len {
                    Some(len) => non_null!(len.eval(record)?)
                        .coerce_to(&DfType::BigInt, len.ty())?
                        .try_into()?,
                    None => s.len() as i64 + 1,
                };

                if len <= 0 {
                    return Ok("".into());
                }

                let start = match from.cmp(&0) {
                    Ordering::Equal => return Ok("".into()),
                    Ordering::Less => {
                        let reverse_from = -from as usize;
                        if reverse_from > s.len() {
                            return Ok("".into());
                        }
                        s.len() - reverse_from
                    }
                    Ordering::Greater => (from - 1) as usize,
                };

                Ok(s.chars()
                    .skip(start)
                    .take(len as _)
                    .collect::<String>()
                    .into())
            }
            BuiltinFunction::Greatest { args, compare_as } => {
                greatest_or_least(args, record, compare_as, ty, |v1, v2| v1 > v2)
            }
            BuiltinFunction::Least { args, compare_as } => {
                greatest_or_least(args, record, compare_as, ty, |v1, v2| v1 < v2)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveTime, Timelike};
    use launchpad::arbitrary::arbitrary_timestamp_naive_date_time;
    use nom_sql::Dialect::*;
    use readyset_data::Collation;
    use test_strategy::proptest;

    use super::*;
    use crate::eval::tests::eval_expr;
    use crate::utils::{make_call, make_column, make_literal};
    use crate::Dialect;

    #[test]
    fn eval_call_convert_tz() {
        let expr = make_call(BuiltinFunction::ConvertTZ {
            args: [make_column(0), make_column(1), make_column(2)],
            subsecond_digits: Dialect::DEFAULT_MYSQL.default_subsecond_digits(),
        });
        let datetime = NaiveDateTime::new(
            NaiveDate::from_ymd(2003, 10, 12),
            NaiveTime::from_hms(5, 13, 33),
        );
        let expected = NaiveDateTime::new(
            NaiveDate::from_ymd(2003, 10, 12),
            NaiveTime::from_hms(11, 58, 33),
        );
        let src = "Atlantic/Cape_Verde";
        let target = "Asia/Kathmandu";
        assert_eq!(
            expr.eval::<DfValue>(&[
                datetime.into(),
                src.try_into().unwrap(),
                target.try_into().unwrap()
            ])
            .unwrap(),
            expected.into()
        );
        assert_eq!(
            expr.eval::<DfValue>(&[
                datetime.into(),
                "invalid timezone".try_into().unwrap(),
                target.try_into().unwrap()
            ])
            .unwrap(),
            DfValue::None
        );
        assert_eq!(
            expr.eval::<DfValue>(&[
                datetime.into(),
                src.try_into().unwrap(),
                "invalid timezone".try_into().unwrap()
            ])
            .unwrap(),
            DfValue::None
        );

        let string_datetime = datetime.to_string();
        assert_eq!(
            expr.eval::<DfValue>(&[
                string_datetime.clone().try_into().unwrap(),
                src.try_into().unwrap(),
                target.try_into().unwrap()
            ])
            .unwrap(),
            expected.into()
        );

        assert_eq!(
            expr.eval::<DfValue>(&[
                string_datetime.clone().try_into().unwrap(),
                "invalid timezone".try_into().unwrap(),
                target.try_into().unwrap()
            ])
            .unwrap(),
            DfValue::None
        );
        assert_eq!(
            expr.eval::<DfValue>(&[
                string_datetime.try_into().unwrap(),
                src.try_into().unwrap(),
                "invalid timezone".try_into().unwrap()
            ])
            .unwrap(),
            DfValue::None
        );
    }

    #[test]
    fn eval_call_day_of_week() {
        let expr = make_call(BuiltinFunction::DayOfWeek(make_column(0)));
        let expected = DfValue::Int(2);

        let date = NaiveDate::from_ymd(2021, 3, 22); // Monday

        assert_eq!(expr.eval::<DfValue>(&[date.into()]).unwrap(), expected);
        assert_eq!(
            expr.eval::<DfValue>(&[date.to_string().try_into().unwrap()])
                .unwrap(),
            expected
        );

        let datetime = NaiveDateTime::new(
            date, // Monday
            NaiveTime::from_hms(18, 8, 00),
        );
        assert_eq!(expr.eval::<DfValue>(&[datetime.into()]).unwrap(), expected);
        assert_eq!(
            expr.eval::<DfValue>(&[datetime.to_string().try_into().unwrap()])
                .unwrap(),
            expected
        );
    }

    #[test]
    fn eval_call_if_null() {
        let expr = make_call(BuiltinFunction::IfNull(make_column(0), make_column(1)));
        let value = DfValue::Int(2);

        assert_eq!(
            expr.eval(&[DfValue::None, DfValue::from(2)]).unwrap(),
            value
        );
        assert_eq!(
            expr.eval(&[DfValue::from(2), DfValue::from(3)]).unwrap(),
            value
        );

        let expr2 = make_call(BuiltinFunction::IfNull(
            make_literal(DfValue::None),
            make_column(0),
        ));
        assert_eq!(expr2.eval::<DfValue>(&[2.into()]).unwrap(), value);

        let expr3 = make_call(BuiltinFunction::IfNull(
            make_column(0),
            make_literal(DfValue::Int(2)),
        ));
        assert_eq!(expr3.eval(&[DfValue::None]).unwrap(), value);
    }

    #[test]
    fn eval_call_month() {
        let expr = make_call(BuiltinFunction::Month(make_column(0)));
        let datetime = NaiveDateTime::new(
            NaiveDate::from_ymd(2003, 10, 12),
            NaiveTime::from_hms(5, 13, 33),
        );
        let expected = 10_u32;
        assert_eq!(
            expr.eval(&[DfValue::from(datetime)]).unwrap(),
            expected.into()
        );
        assert_eq!(
            expr.eval::<DfValue>(&[datetime.to_string().try_into().unwrap()])
                .unwrap(),
            expected.into()
        );
        assert_eq!(
            expr.eval::<DfValue>(&[datetime.date().into()]).unwrap(),
            expected.into()
        );
        assert_eq!(
            expr.eval::<DfValue>(&[datetime.date().to_string().try_into().unwrap()])
                .unwrap(),
            expected.into()
        );
        assert_eq!(
            expr.eval::<DfValue>(&["invalid date".try_into().unwrap()])
                .unwrap(),
            DfValue::None
        );
    }

    #[test]
    fn eval_call_timediff() {
        let expr = make_call(BuiltinFunction::Timediff(make_column(0), make_column(1)));
        let param1 = NaiveDateTime::new(
            NaiveDate::from_ymd(2003, 10, 12),
            NaiveTime::from_hms(5, 13, 33),
        );
        let param2 = NaiveDateTime::new(
            NaiveDate::from_ymd(2003, 10, 14),
            NaiveTime::from_hms(4, 13, 33),
        );
        assert_eq!(
            expr.eval::<DfValue>(&[param1.into(), param2.into()])
                .unwrap(),
            DfValue::Time(MySqlTime::from_hmsus(false, 47, 0, 0, 0))
        );
        assert_eq!(
            expr.eval::<DfValue>(&[
                param1.to_string().try_into().unwrap(),
                param2.to_string().try_into().unwrap()
            ])
            .unwrap(),
            DfValue::Time(MySqlTime::from_hmsus(false, 47, 0, 0, 0))
        );
        let param1 = NaiveDateTime::new(
            NaiveDate::from_ymd(2003, 10, 12),
            NaiveTime::from_hms(5, 13, 33),
        );
        let param2 = NaiveDateTime::new(
            NaiveDate::from_ymd(2003, 10, 10),
            NaiveTime::from_hms(4, 13, 33),
        );
        assert_eq!(
            expr.eval::<DfValue>(&[param1.into(), param2.into()])
                .unwrap(),
            DfValue::Time(MySqlTime::from_hmsus(true, 49, 0, 0, 0))
        );
        assert_eq!(
            expr.eval::<DfValue>(&[
                param1.to_string().try_into().unwrap(),
                param2.to_string().try_into().unwrap()
            ])
            .unwrap(),
            DfValue::Time(MySqlTime::from_hmsus(true, 49, 0, 0, 0))
        );
        let param2 = NaiveTime::from_hms(4, 13, 33);
        assert_eq!(
            expr.eval::<DfValue>(&[param1.into(), param2.into()])
                .unwrap(),
            DfValue::None
        );
        assert_eq!(
            expr.eval::<DfValue>(&[
                param1.to_string().try_into().unwrap(),
                param2.to_string().try_into().unwrap()
            ])
            .unwrap(),
            DfValue::None
        );
        let param1 = NaiveTime::from_hms(5, 13, 33);
        assert_eq!(
            expr.eval::<DfValue>(&[param1.into(), param2.into()])
                .unwrap(),
            DfValue::Time(MySqlTime::from_hmsus(true, 1, 0, 0, 0))
        );
        assert_eq!(
            expr.eval::<DfValue>(&[
                param1.to_string().try_into().unwrap(),
                param2.to_string().try_into().unwrap()
            ])
            .unwrap(),
            DfValue::Time(MySqlTime::from_hmsus(true, 1, 0, 0, 0))
        );
        let param1 = "not a date nor time";
        let param2 = "01:00:00.4";
        assert_eq!(
            expr.eval::<DfValue>(&[param1.try_into().unwrap(), param2.try_into().unwrap()])
                .unwrap(),
            DfValue::Time(MySqlTime::from_hmsus(false, 1, 0, 0, 400_000))
        );
        assert_eq!(
            expr.eval::<DfValue>(&[param2.try_into().unwrap(), param1.try_into().unwrap()])
                .unwrap(),
            DfValue::Time(MySqlTime::from_hmsus(true, 1, 0, 0, 400_000))
        );

        let param2 = "10000.4";
        assert_eq!(
            expr.eval::<DfValue>(&[param1.try_into().unwrap(), param2.try_into().unwrap()])
                .unwrap(),
            DfValue::Time(MySqlTime::from_hmsus(false, 1, 0, 0, 400_000))
        );
        assert_eq!(
            expr.eval::<DfValue>(&[param2.try_into().unwrap(), param1.try_into().unwrap()])
                .unwrap(),
            DfValue::Time(MySqlTime::from_hmsus(true, 1, 0, 0, 400_000))
        );

        let param2: f32 = 3.57;
        assert_eq!(
            expr.eval::<DfValue>(&[
                DfValue::try_from(param1).unwrap(),
                DfValue::try_from(param2).unwrap()
            ])
            .unwrap(),
            DfValue::Time(MySqlTime::from_microseconds(
                (-param2 * 1_000_000_f32) as i64
            ))
        );

        let param2: f64 = 3.57;
        assert_eq!(
            expr.eval::<DfValue>(&[
                DfValue::try_from(param1).unwrap(),
                DfValue::try_from(param2).unwrap()
            ])
            .unwrap(),
            DfValue::Time(MySqlTime::from_microseconds(
                (-param2 * 1_000_000_f64) as i64
            ))
        );
    }

    #[test]
    fn eval_call_addtime() {
        let expr = make_call(BuiltinFunction::Addtime(make_column(0), make_column(1)));
        let param1 = NaiveDateTime::new(
            NaiveDate::from_ymd(2003, 10, 12),
            NaiveTime::from_hms(5, 13, 33),
        );
        let param2 = NaiveDateTime::new(
            NaiveDate::from_ymd(2003, 10, 14),
            NaiveTime::from_hms(4, 13, 33),
        );
        assert_eq!(
            expr.eval::<DfValue>(&[param1.into(), param2.into()])
                .unwrap(),
            DfValue::None
        );
        assert_eq!(
            expr.eval::<DfValue>(&[
                param1.to_string().try_into().unwrap(),
                param2.to_string().try_into().unwrap()
            ])
            .unwrap(),
            DfValue::None
        );
        let param2 = NaiveTime::from_hms(4, 13, 33);
        assert_eq!(
            expr.eval::<DfValue>(&[param1.into(), param2.into()])
                .unwrap(),
            DfValue::TimestampTz(
                NaiveDateTime::new(
                    NaiveDate::from_ymd(2003, 10, 12),
                    NaiveTime::from_hms(9, 27, 6),
                )
                .into()
            )
        );
        assert_eq!(
            expr.eval::<DfValue>(&[
                param1.to_string().try_into().unwrap(),
                param2.to_string().try_into().unwrap()
            ])
            .unwrap(),
            DfValue::TimestampTz(
                NaiveDateTime::new(
                    NaiveDate::from_ymd(2003, 10, 12),
                    NaiveTime::from_hms(9, 27, 6),
                )
                .into()
            )
        );
        let param2 = MySqlTime::from_hmsus(false, 3, 11, 35, 0);
        assert_eq!(
            expr.eval::<DfValue>(&[param1.into(), param2.into()])
                .unwrap(),
            DfValue::TimestampTz(
                NaiveDateTime::new(
                    NaiveDate::from_ymd(2003, 10, 12),
                    NaiveTime::from_hms(2, 1, 58),
                )
                .into()
            )
        );
        assert_eq!(
            expr.eval::<DfValue>(&[
                param1.to_string().try_into().unwrap(),
                param2.to_string().try_into().unwrap()
            ])
            .unwrap(),
            DfValue::TimestampTz(
                NaiveDateTime::new(
                    NaiveDate::from_ymd(2003, 10, 12),
                    NaiveTime::from_hms(2, 1, 58),
                )
                .into()
            )
        );
        let param1 = MySqlTime::from_hmsus(true, 10, 12, 44, 123_000);
        assert_eq!(
            expr.eval::<DfValue>(&[param2.into(), param1.into()])
                .unwrap(),
            DfValue::Time(MySqlTime::from_hmsus(true, 7, 1, 9, 123_000))
        );
        assert_eq!(
            expr.eval::<DfValue>(&[
                param2.to_string().try_into().unwrap(),
                param1.to_string().try_into().unwrap()
            ])
            .unwrap(),
            DfValue::Time(MySqlTime::from_hmsus(true, 7, 1, 9, 123_000))
        );
        let param1 = "not a date nor time";
        let param2 = "01:00:00.4";
        assert_eq!(
            expr.eval::<DfValue>(&[param1.try_into().unwrap(), param2.try_into().unwrap()])
                .unwrap(),
            DfValue::Time(MySqlTime::from_hmsus(true, 1, 0, 0, 400_000))
        );
        assert_eq!(
            expr.eval::<DfValue>(&[param2.try_into().unwrap(), param1.try_into().unwrap()])
                .unwrap(),
            DfValue::Time(MySqlTime::from_hmsus(true, 1, 0, 0, 400_000))
        );

        let param2 = "10000.4";
        assert_eq!(
            expr.eval::<DfValue>(&[param1.try_into().unwrap(), param2.try_into().unwrap()])
                .unwrap(),
            DfValue::Time(MySqlTime::from_hmsus(true, 1, 0, 0, 400_000))
        );
        assert_eq!(
            expr.eval::<DfValue>(&[param2.try_into().unwrap(), param1.try_into().unwrap()])
                .unwrap(),
            DfValue::Time(MySqlTime::from_hmsus(true, 1, 0, 0, 400_000))
        );

        let param2: f32 = 3.57;
        assert_eq!(
            expr.eval::<DfValue>(&[
                param1.try_into().unwrap(),
                DfValue::try_from(param2).unwrap()
            ])
            .unwrap(),
            DfValue::Time(MySqlTime::from_microseconds(
                (param2 * 1_000_000_f32) as i64
            ))
        );

        let param2: f64 = 3.57;
        assert_eq!(
            expr.eval::<DfValue>(&[
                param1.try_into().unwrap(),
                DfValue::try_from(param2).unwrap()
            ])
            .unwrap(),
            DfValue::Time(MySqlTime::from_microseconds(
                (param2 * 1_000_000_f64) as i64
            ))
        );
    }

    #[test]
    fn eval_call_round() {
        let expr = make_call(BuiltinFunction::Round(make_column(0), make_column(1)));
        let number: f64 = 4.12345;
        let precision = 3;
        let param1 = DfValue::try_from(number).unwrap();
        let param2 = DfValue::Int(precision);
        let want = DfValue::try_from(4.123_f64).unwrap();
        assert_eq!(
            expr.eval::<DfValue>(&[param1, param2.clone()]).unwrap(),
            want
        );

        let number: f32 = 4.12345;
        let param1 = DfValue::try_from(number).unwrap();
        let want = DfValue::try_from(4.123_f32).unwrap();
        assert_eq!(expr.eval::<DfValue>(&[param1, param2]).unwrap(), want);
    }

    #[test]
    fn eval_call_round_with_negative_precision() {
        let expr = make_call(BuiltinFunction::Round(make_column(0), make_column(1)));
        let number: f64 = 52.12345;
        let precision = -1;
        let param1 = DfValue::try_from(number).unwrap();
        let param2 = DfValue::Int(precision);
        let want = DfValue::try_from(50.0).unwrap();
        assert_eq!(
            expr.eval::<DfValue>(&[param1, param2.clone()]).unwrap(),
            want
        );

        let number: f32 = 52.12345;
        let param1 = DfValue::try_from(number).unwrap();
        assert_eq!(expr.eval::<DfValue>(&[param1, param2]).unwrap(), want);
    }

    #[test]
    fn eval_call_round_with_float_precision() {
        let expr = make_call(BuiltinFunction::Round(make_column(0), make_column(1)));
        let number: f32 = 52.12345;
        let precision = -1.0_f64;
        let param1 = DfValue::try_from(number).unwrap();
        let param2 = DfValue::try_from(precision).unwrap();
        let want = DfValue::try_from(50.0).unwrap();
        assert_eq!(
            expr.eval::<DfValue>(&[param1, param2.clone()]).unwrap(),
            want,
        );

        let number: f64 = 52.12345;
        let param1 = DfValue::try_from(number).unwrap();
        assert_eq!(expr.eval::<DfValue>(&[param1, param2]).unwrap(), want);
    }

    // This is actually straight from MySQL:
    // mysql> SELECT ROUND(123.3, "banana");
    // +------------------------+
    // | ROUND(123.3, "banana") |
    // +------------------------+
    // |                    123 |
    // +------------------------+
    // 1 row in set, 2 warnings (0.00 sec)
    #[test]
    fn eval_call_round_with_banana() {
        let expr = make_call(BuiltinFunction::Round(make_column(0), make_column(1)));
        let number: f32 = 52.12345;
        let precision = "banana";
        let param1 = DfValue::try_from(number).unwrap();
        let param2 = DfValue::try_from(precision).unwrap();
        let want = DfValue::try_from(52.).unwrap();
        assert_eq!(
            expr.eval::<DfValue>(&[param1, param2.clone()]).unwrap(),
            want,
        );

        let number: f64 = 52.12345;
        let param1 = DfValue::try_from(number).unwrap();
        assert_eq!(expr.eval::<DfValue>(&[param1, param2]).unwrap(), want,);
    }

    #[test]
    fn eval_call_round_with_decimal() {
        let expr = make_call(BuiltinFunction::Round(make_column(0), make_column(1)));
        assert_eq!(
            expr.eval::<DfValue>(&[
                DfValue::from(Decimal::from_f64(52.123).unwrap()),
                DfValue::from(1)
            ])
            .unwrap(),
            DfValue::from(Decimal::from_f64(52.1)),
        );

        assert_eq!(
            expr.eval::<DfValue>(&[
                DfValue::from(Decimal::from_f64(-52.666).unwrap()),
                DfValue::from(2)
            ])
            .unwrap(),
            DfValue::from(Decimal::from_f64(-52.67)),
        );

        assert_eq!(
            expr.eval::<DfValue>(&[
                DfValue::from(Decimal::from_f64(-52.666).unwrap()),
                DfValue::from(-1)
            ])
            .unwrap(),
            DfValue::from(Decimal::from_f64(-50.)),
        );
    }

    #[test]
    fn eval_call_round_with_strings() {
        let expr = make_call(BuiltinFunction::Round(make_column(0), make_column(1)));
        assert_eq!(
            expr.eval::<DfValue>(&[DfValue::from("52.123"), DfValue::from(1)])
                .unwrap(),
            DfValue::try_from(52.1).unwrap(),
        );

        assert_eq!(
            expr.eval::<DfValue>(&[DfValue::from("-52.666banana"), DfValue::from(2)])
                .unwrap(),
            DfValue::try_from(-52.67).unwrap(),
        );

        assert_eq!(
            expr.eval::<DfValue>(&[DfValue::from("-52.666banana"), DfValue::from(-1)])
                .unwrap(),
            DfValue::try_from(-50.).unwrap(),
        );
    }

    #[test]
    fn eval_call_json_typeof() {
        let examples = [
            ("null", "null"),
            ("true", "boolean"),
            ("false", "boolean"),
            ("123", "number"),
            (r#""hello""#, "string"),
            (r#"["hello", 123]"#, "array"),
            (r#"{ "hello": "world", "abc": 123 }"#, "object"),
        ];

        let expr = make_call(BuiltinFunction::JsonTypeof(make_column(0)));

        for (json, expected_type) in examples {
            let json_type = expr.eval::<DfValue>(&[json.into()]).unwrap();
            assert_eq!(json_type, DfValue::from(expected_type));
        }
    }

    #[test]
    fn month_null() {
        let expr = make_call(BuiltinFunction::Month(make_column(0)));
        assert_eq!(
            expr.eval::<DfValue>(&[DfValue::None]).unwrap(),
            DfValue::None
        );
    }

    // NOTE(Fran): We have to be careful when testing timezones, as the time difference
    //   between two timezones might differ depending on the date (due to daylight savings
    //   or by historical changes).
    #[proptest]
    fn convert_tz(#[strategy(arbitrary_timestamp_naive_date_time())] datetime: NaiveDateTime) {
        let src = "Atlantic/Cape_Verde";
        let target = "Asia/Kathmandu";
        let src_tz: Tz = src.parse().unwrap();
        let target_tz: Tz = target.parse().unwrap();
        let expected = src_tz
            .yo_opt(datetime.year(), datetime.ordinal())
            .and_hms_opt(datetime.hour(), datetime.minute(), datetime.second())
            .unwrap()
            .with_timezone(&target_tz)
            .naive_local();
        assert_eq!(super::convert_tz(&datetime, src, target).unwrap(), expected);
        super::convert_tz(&datetime, "invalid timezone", target).unwrap_err();
        assert!(super::convert_tz(&datetime, src, "invalid timezone").is_err());
    }

    #[proptest]
    fn day_of_week(#[strategy(arbitrary_timestamp_naive_date_time())] datetime: NaiveDateTime) {
        let expected = datetime.weekday().number_from_sunday() as u8;
        assert_eq!(super::day_of_week(&datetime.date()), expected);
    }

    #[proptest]
    fn month(#[strategy(arbitrary_timestamp_naive_date_time())] datetime: NaiveDateTime) {
        let expected = datetime.month() as u8;
        assert_eq!(super::month(&datetime.date()), expected);
    }

    #[test]
    fn coalesce() {
        let expr = Expr::Call {
            func: Box::new(BuiltinFunction::Coalesce(
                Expr::Column {
                    index: 0,
                    ty: DfType::Unknown,
                },
                vec![Expr::Literal {
                    val: 1.into(),
                    ty: DfType::Int,
                }],
            )),
            ty: DfType::Unknown,
        };
        let call_with = |val: DfValue| expr.eval(&[val]);

        assert_eq!(call_with(DfValue::None).unwrap(), 1.into());
        assert_eq!(call_with(123.into()).unwrap(), 123.into());
    }

    #[test]
    fn coalesce_more_args() {
        let expr = Expr::Call {
            func: Box::new(BuiltinFunction::Coalesce(
                Expr::Column {
                    index: 0,
                    ty: DfType::Unknown,
                },
                vec![
                    Expr::Column {
                        index: 1,
                        ty: DfType::Unknown,
                    },
                    Expr::Literal {
                        val: 1.into(),
                        ty: DfType::Int,
                    },
                ],
            )),
            ty: DfType::Unknown,
        };
        let call_with = |val1: DfValue, val2: DfValue| expr.eval(&[val1, val2]);

        assert_eq!(call_with(DfValue::None, DfValue::None).unwrap(), 1.into());
        assert_eq!(
            call_with(DfValue::None, "abc".into()).unwrap(),
            "abc".into()
        );
        assert_eq!(call_with(123.into(), DfValue::None).unwrap(), 123.into());
        assert_eq!(call_with(123.into(), 456.into()).unwrap(), 123.into());
    }

    #[test]
    fn concat() {
        let expr = Expr::Call {
            func: Box::new(BuiltinFunction::Concat(
                Expr::Literal {
                    val: "My".into(),
                    ty: DfType::Text(Collation::default()),
                },
                vec![
                    Expr::Literal {
                        val: "S".into(),
                        ty: DfType::Text(Collation::default()),
                    },
                    Expr::Literal {
                        val: "QL".into(),
                        ty: DfType::Text(Collation::default()),
                    },
                ],
            )),
            ty: DfType::Text(Collation::default()),
        };

        let res = expr.eval::<DfValue>(&[]).unwrap();
        assert_eq!(res, "MySQL".into());
    }

    #[test]
    fn concat_with_nulls() {
        let expr = Expr::Call {
            func: Box::new(BuiltinFunction::Concat(
                Expr::Literal {
                    val: "My".into(),
                    ty: DfType::Text(Collation::default()),
                },
                vec![
                    Expr::Literal {
                        val: DfValue::None,
                        ty: DfType::Text(Collation::default()),
                    },
                    Expr::Literal {
                        val: "QL".into(),
                        ty: DfType::Text(Collation::default()),
                    },
                ],
            )),
            ty: DfType::Text(Collation::default()),
        };

        let res = expr.eval::<DfValue>(&[]).unwrap();
        assert_eq!(res, DfValue::None);
    }

    #[test]
    fn substring_with_from_and_for() {
        let expr = Expr::Call {
            func: Box::new(BuiltinFunction::Substring(
                Expr::Literal {
                    val: "abcdef".into(),
                    ty: DfType::Text(Collation::default()),
                },
                Some(Expr::Column {
                    index: 0,
                    ty: DfType::Int,
                }),
                Some(Expr::Column {
                    index: 1,
                    ty: DfType::Int,
                }),
            )),
            ty: DfType::Text(Collation::default()),
        };
        let call_with =
            |from: i64, len: i64| expr.eval::<DfValue>(&[from.into(), len.into()]).unwrap();

        assert_eq!(call_with(2, 3), "bcd".into());
        assert_eq!(call_with(3, 3), "cde".into());
        assert_eq!(call_with(6, 3), "f".into());
        assert_eq!(call_with(7, 12), "".into());
        assert_eq!(call_with(-3, 3), "def".into());
        assert_eq!(call_with(-3, -3), "".into());
        assert_eq!(call_with(0, 3), "".into());
        assert_eq!(call_with(0, 0), "".into());
        assert_eq!(call_with(-7, 2), "".into());
    }

    #[test]
    fn substring_multibyte() {
        let expr = Expr::Call {
            func: Box::new(BuiltinFunction::Substring(
                Expr::Literal {
                    val: "é".into(),
                    ty: DfType::Text(Collation::default()),
                },
                Some(Expr::Literal {
                    val: 1.into(),
                    ty: DfType::Int,
                }),
                Some(Expr::Literal {
                    val: 1.into(),
                    ty: DfType::Int,
                }),
            )),
            ty: DfType::Text(Collation::default()),
        };
        let res = expr.eval::<DfValue>(&[]).unwrap();
        assert_eq!(res, "é".into());
    }

    #[test]
    fn substring_with_from() {
        let expr = Expr::Call {
            func: Box::new(BuiltinFunction::Substring(
                Expr::Literal {
                    val: "abcdef".into(),
                    ty: DfType::Text(Collation::default()),
                },
                Some(Expr::Column {
                    index: 0,
                    ty: DfType::Int,
                }),
                None,
            )),
            ty: DfType::Text(Collation::default()),
        };
        let res = expr.eval::<DfValue>(&[2.into()]).unwrap();
        assert_eq!(res, "bcdef".into());
    }

    #[test]
    fn substring_with_for() {
        let expr = Expr::Call {
            func: Box::new(BuiltinFunction::Substring(
                Expr::Literal {
                    val: "abcdef".into(),
                    ty: DfType::Text(Collation::default()),
                },
                None,
                Some(Expr::Column {
                    index: 0,
                    ty: DfType::Int,
                }),
            )),
            ty: DfType::Text(Collation::default()),
        };
        let res = expr.eval::<DfValue>(&[3.into()]).unwrap();
        assert_eq!(res, "abc".into());
    }

    #[test]
    fn greatest_mysql() {
        assert_eq!(eval_expr("greatest(1, 2, 3)", MySQL), 3.into());
        assert_eq!(
            eval_expr("greatest(123, '23')", MySQL),
            23.into() // TODO(ENG-1911) this should be a string!
        );
        assert_eq!(
            eval_expr("greatest(1.23, '23')", MySQL),
            (23.0).try_into().unwrap()
        );
    }

    #[test]
    fn least_mysql() {
        assert_eq!(eval_expr("least(1, 2, 3)", MySQL), 1u64.into());
        assert_eq!(
            eval_expr("least(123, '23')", MySQL),
            123.into() // TODO(ENG-1911) this should be a string!
        );
        assert_eq!(
            eval_expr("least(1.23, '23')", MySQL),
            (1.23_f64).try_into().unwrap() // TODO(ENG-1911) this should be a string!
        );
    }

    #[test]
    #[ignore = "ENG-1909"]
    fn greatest_mysql_ints_and_floats() {
        assert_eq!(
            eval_expr("greatest(1, 2.5, 3)", MySQL),
            (3.0f64).try_into().unwrap()
        );
    }

    #[test]
    fn greatest_postgresql() {
        assert_eq!(eval_expr("greatest(1,2,3)", PostgreSQL), 3.into());
        assert_eq!(eval_expr("greatest(123, '23')", PostgreSQL), 123.into());
        assert_eq!(eval_expr("greatest(23, '123')", PostgreSQL), 123.into());
    }

    #[test]
    fn least_postgresql() {
        assert_eq!(eval_expr("least(1,2,3)", PostgreSQL), 1.into());
        assert_eq!(eval_expr("least(123, '23')", PostgreSQL), 23.into());
    }
}