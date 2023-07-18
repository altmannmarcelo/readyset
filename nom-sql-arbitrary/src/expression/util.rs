//! Utility macros to be used for common [`Expr`] generation.

use nom_sql::{CaseWhenBranch, Expr, FunctionExpr, Literal, SqlType};
use proptest::prop_oneof;
use proptest::strategy::{Just, Strategy};

/// Generates an [`Expr::Cast`] with the form of:
/// ```sql
/// CAST(e, t)
/// ```
/// Where:
/// e: The `Expr` generated with the provided `Strategy`.
/// t: The type to cast the `Expr` to.
///
/// $e: The `Strategy` that generates an `Expr`.
/// $ty: The type to cast `Expr`.
#[allow(dead_code)]
pub fn cast<S>(expr: S, ty: SqlType, postgres_style: bool) -> impl Strategy<Value = Expr>
where
    S: Strategy<Value = Expr>,
{
    expr.prop_map(move |e| Expr::Cast {
        expr: Box::new(e),
        ty: ty.clone(),
        postgres_style,
    })
}

/// Generates an [`Expr::CaseWhen`] with the form of:
/// ```sql
/// CASE
/// WHEN b1 THEN e1
/// WHEN b2 THEN e2
/// ...
/// ELSE en
/// ```
/// Where:
/// b: Boolean `Expr` generated with the provided boolean `Strategy`.
/// e: `Expr` generated with the provided `Strategy`.
/// The amount of cases generated is also random.
///
/// $e: The `Strategy` that generates an `Expr`.
/// $bool: The `Strategy` that generates *boolean* `Expr`.
/// $ty: The type of the resulting `Expr` (the type of the `Expr` generated by $e).
#[allow(dead_code)]
pub fn case_when<S>(expr: S, bool: S) -> impl Strategy<Value = Expr>
where
    S: Strategy<Value = Expr> + Clone,
{
    let branch = (expr.clone(), bool).prop_map(|(e, b)| CaseWhenBranch {
        condition: b,
        body: e,
    });
    let branches = proptest::collection::vec(branch, 1..=3);
    (branches, proptest::option::of(expr.prop_map(Box::new))).prop_map(|(branches, else_expr)| {
        Expr::CaseWhen {
            branches,
            else_expr,
        }
    })
}

/// Generates an [`Expr::Call`] with [`FunctionExpr::Call`] with the form of
/// ```sql
/// IFNULL(e1, e2)
/// ```
/// Where:
/// e1: Either an `Expr` generated by the provided `Strategy`, or `null` (`Literal::Null`).
/// e2: Non-null `Expr` generated by the provided `Strategy`.
///
/// $e: The `Strategy` that generates an `Expr`.
/// $ty: The type of the resulting `Expr` (the type of the `Expr` generated by $e).
#[allow(dead_code)]
pub fn if_null<S>(expr: S) -> impl Strategy<Value = Expr>
where
    S: Strategy<Value = Expr> + Clone,
{
    let expr_or_null = prop_oneof![Just(Expr::Literal(Literal::Null)), expr.clone()];
    (expr_or_null, expr).prop_map(|(e1, e2)| {
        Expr::Call(FunctionExpr::Call {
            name: "ifnull".into(),
            arguments: vec![e1, e2],
        })
    })
}

/// Generates an [`Expr::Call`] with [`FunctionExpr::Call`], in the form of:
/// ```sql
/// COALESCE(e1, .., en)
/// ```
/// Where:
/// e: `Expr` generated with the provided `Strategy`, or `null` (`Literal::Null`). The last
/// expression (en) is never `null`.
///
/// $e: The `Strategy` that generates an `Expr`.
/// $ty: The type of the resulting `Expr` (the type of the `Expr` generated by $e).
#[allow(dead_code)]
pub fn coalesce<S>(expr: S) -> impl Strategy<Value = Expr>
where
    S: Strategy<Value = Expr> + Clone,
{
    let expr_or_null = prop_oneof![Just(Expr::Literal(Literal::Null)), expr.clone()];
    let arguments = proptest::collection::vec(expr_or_null, 1..=5);
    (arguments, expr).prop_map(|(mut arguments, expr)| {
        // This guarantees that the final element is not null
        arguments.push(expr);
        Expr::Call(FunctionExpr::Call {
            name: "coalesce".into(),
            arguments,
        })
    })
}