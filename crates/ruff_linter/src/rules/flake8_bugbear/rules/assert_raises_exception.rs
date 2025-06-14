use std::fmt;

use ruff_macros::{ViolationMetadata, derive_message_formats};
use ruff_python_ast::{self as ast, Expr, WithItem};
use ruff_text_size::Ranged;

use crate::Violation;
use crate::checkers::ast::Checker;

/// ## What it does
/// Checks for `assertRaises` and `pytest.raises` context managers that catch
/// `Exception` or `BaseException`.
///
/// ## Why is this bad?
/// These forms catch every `Exception`, which can lead to tests passing even
/// if, e.g., the code under consideration raises a `SyntaxError` or
/// `IndentationError`.
///
/// Either assert for a more specific exception (builtin or custom), or use
/// `assertRaisesRegex` or `pytest.raises(..., match=<REGEX>)` respectively.
///
/// ## Example
/// ```python
/// self.assertRaises(Exception, foo)
/// ```
///
/// Use instead:
/// ```python
/// self.assertRaises(SomeSpecificException, foo)
/// ```
#[derive(ViolationMetadata)]
pub(crate) struct AssertRaisesException {
    exception: ExceptionKind,
}

impl Violation for AssertRaisesException {
    #[derive_message_formats]
    fn message(&self) -> String {
        let AssertRaisesException { exception } = self;
        format!("Do not assert blind exception: `{exception}`")
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ExceptionKind {
    BaseException,
    Exception,
}

impl fmt::Display for ExceptionKind {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ExceptionKind::BaseException => fmt.write_str("BaseException"),
            ExceptionKind::Exception => fmt.write_str("Exception"),
        }
    }
}

/// B017
pub(crate) fn assert_raises_exception(checker: &Checker, items: &[WithItem]) {
    for item in items {
        let Expr::Call(ast::ExprCall {
            func,
            arguments,
            range: _,
            node_index: _,
        }) = &item.context_expr
        else {
            continue;
        };

        if item.optional_vars.is_some() {
            continue;
        }

        let [arg] = &*arguments.args else {
            continue;
        };

        let semantic = checker.semantic();

        let Some(builtin_symbol) = semantic.resolve_builtin_symbol(arg) else {
            continue;
        };

        let exception = match builtin_symbol {
            "Exception" => ExceptionKind::Exception,
            "BaseException" => ExceptionKind::BaseException,
            _ => continue,
        };

        if !(matches!(func.as_ref(), Expr::Attribute(ast::ExprAttribute { attr, .. }) if attr == "assertRaises")
            || semantic
                .resolve_qualified_name(func)
                .is_some_and(|qualified_name| {
                    matches!(qualified_name.segments(), ["pytest", "raises"])
                })
                && arguments.find_keyword("match").is_none())
        {
            continue;
        }

        checker.report_diagnostic(AssertRaisesException { exception }, item.range());
    }
}
