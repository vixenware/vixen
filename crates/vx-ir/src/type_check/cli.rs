//! CLI-literal type checking (`@cc[ … ]`).
//!
//! For now: type-check the interpolation holes so `$name` / `${expr}` are real,
//! checked sub-expressions. The literal's own type (a `Command` value) is not
//! modeled yet, so it is `Poison`. Validating the command *against its grammar*
//! — unknown flags, value domains, the typed expectation of each hole — lands in
//! a later phase, once package resolution makes the grammar value reachable from
//! here.

use vx_types::TypeId;

use crate::ExprId;
use crate::expr::{CliLiteralExpr, StringPart};

use super::TypeChecker;

pub(crate) fn infer_cli_literal(
    tc: &mut TypeChecker,
    _expr_id: ExprId,
    cli: &CliLiteralExpr,
) -> TypeId {
    for arg in &cli.args {
        for piece in &arg.pieces {
            if let StringPart::Expr(id) = piece {
                tc.infer_expr(*id);
            }
        }
    }
    tc.poison()
}
