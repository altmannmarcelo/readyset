use noria_errors::ReadySetResult;

use crate::column::Column;
use crate::query::MirQuery;
use crate::MirNodeRef;

fn has_column(n: &MirNodeRef, column: &Column) -> bool {
    if n.borrow().columns().contains(column) {
        return true;
    } else {
        for a in n.borrow().ancestors().iter().map(|n| n.upgrade().unwrap()) {
            if has_column(&a, column) {
                return true;
            }
        }
    }
    false
}

pub(super) fn pull_columns_for(n: MirNodeRef) -> ReadySetResult<()> {
    let mut queue = vec![n];

    while let Some(mn) = queue.pop() {
        // a node needs all of the columns it projects into its output
        // however, it may also need *additional* columns to perform its functionality; consider,
        // e.g., a filter that filters on a column that it doesn't project
        let needed_columns: Vec<Column> = mn
            .borrow()
            .referenced_columns()
            .into_iter()
            .filter(|c| {
                !mn.borrow()
                    .ancestors()
                    .iter()
                    .map(|n| n.upgrade().unwrap())
                    .any(|a| a.borrow().columns().iter().any(|ac| ac == c))
            })
            .collect();

        let mut found: Vec<&Column> = Vec::new();
        for ancestor in mn.borrow().ancestors().iter().map(|n| n.upgrade().unwrap()) {
            if ancestor.borrow().ancestors().is_empty() {
                // base, do nothing
                continue;
            }
            for c in &needed_columns {
                if !found.contains(&c) && has_column(&ancestor, c) {
                    ancestor.borrow_mut().add_column(c.clone())?;
                    found.push(c);
                }
            }
            queue.push(ancestor.clone());
        }
    }

    Ok(())
}

pub(super) fn pull_all_required_columns(q: &mut MirQuery) -> ReadySetResult<()> {
    pull_columns_for(q.leaf.clone())
}

#[cfg(test)]
mod tests {
    use dataflow::ops::grouped::aggregate::Aggregation;
    use nom_sql::{
        BinaryOperator, ColumnSpecification, Expression, FunctionExpression, Literal, SqlType,
    };

    use super::*;
    use crate::node::node_inner::MirNodeInner;
    use crate::node::MirNode;

    #[test]
    fn changing_index() {
        let base = MirNode::new(
            "base".into(),
            0,
            vec!["a".into(), "b".into(), "c".into()],
            MirNodeInner::Base {
                column_specs: vec![
                    (
                        ColumnSpecification {
                            column: nom_sql::Column::from("a"),
                            sql_type: SqlType::Int(None),
                            constraints: vec![],
                            comment: None,
                        },
                        None,
                    ),
                    (
                        ColumnSpecification {
                            column: nom_sql::Column::from("b"),
                            sql_type: SqlType::Int(None),
                            constraints: vec![],
                            comment: None,
                        },
                        None,
                    ),
                    (
                        ColumnSpecification {
                            column: nom_sql::Column::from("c"),
                            sql_type: SqlType::Int(None),
                            constraints: vec![],
                            comment: None,
                        },
                        None,
                    ),
                ],
                primary_key: Some([Column::from("a")].into()),
                unique_keys: Default::default(),
                adapted_over: None,
            },
            vec![],
            vec![],
        );

        // SUM(b)
        let grp = MirNode::new(
            "grp".into(),
            0,
            vec!["agg".into()],
            MirNodeInner::Aggregation {
                on: "b".into(),
                group_by: vec![],
                kind: Aggregation::Sum,
            },
            vec![MirNodeRef::downgrade(&base)],
            vec![],
        );

        let condition_expression_1 = Expression::BinaryOp {
            op: BinaryOperator::Equal,
            lhs: Box::new(Expression::Column("a".into())),
            rhs: Box::new(Expression::Literal(Literal::Integer(1))),
        };

        // σ[a = 1]
        let fil = MirNode::new(
            "fil".into(),
            0,
            vec!["a".into(), "agg".into()],
            MirNodeInner::Filter {
                conditions: condition_expression_1.clone(),
            },
            vec![MirNodeRef::downgrade(&grp)],
            vec![],
        );

        // a, agg, IFNULL(c, 0) as c0
        let prj = MirNode::new(
            "prj".into(),
            0,
            vec!["a".into(), "agg".into(), "c0".into()],
            MirNodeInner::Project {
                emit: vec!["a".into(), "agg".into()],
                expressions: vec![(
                    "c0".into(),
                    Expression::Call(FunctionExpression::Call {
                        name: "ifnull".into(),
                        arguments: vec![
                            Expression::Column("c".into()),
                            Expression::Literal(0.into()),
                        ],
                    }),
                )],
                literals: vec![],
            },
            vec![MirNodeRef::downgrade(&fil)],
            vec![],
        );

        let mut query = MirQuery {
            name: "changing_index".into(),
            roots: vec![base],
            leaf: prj,
        };

        pull_all_required_columns(&mut query).unwrap();

        assert_eq!(
            grp.borrow().columns(),
            &[Column::from("c"), Column::from("a"), Column::from("agg")]
        );

        // The filter has to add the column in the same place as the aggregate
        assert_eq!(
            fil.borrow().columns(),
            &[Column::from("c"), Column::from("a"), Column::from("agg")]
        );

        // The filter has to filter on the correct field
        match &fil.borrow().inner {
            MirNodeInner::Filter { conditions, .. } => {
                assert_eq!(conditions, &condition_expression_1)
            }
            _ => unreachable!(),
        };
    }
}