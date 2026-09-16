use crate::annotations::{CONDITION, RECOGNIZED_NAMES, SUPPRESS};
use omega_parser::prelude::{AnnotationArg, AnnotationNode, Ident, Origin, Span};
use std::fmt;

#[derive(Debug, Clone, Default)]
pub struct SourceAnnotations {
    pub suppress: Vec<Ident>,
}

#[derive(Debug, Clone)]
pub struct SourceAnnotationError {
    pub span: Span,
    pub origin: Origin,
    pub kind: SourceAnnotationErrorKind,
}

#[derive(Debug, Clone)]
pub enum SourceAnnotationErrorKind {
    Duplicate { name: Ident, first: Span },
    Unknown { name: Ident },
    NotSourceLevel { name: Ident },
    InvalidArguments { name: Ident, reason: String },
}

impl SourceAnnotationError {
    fn new(annotation: &AnnotationNode, kind: SourceAnnotationErrorKind) -> Self {
        Self {
            span: annotation.span,
            origin: annotation.origin,
            kind,
        }
    }

    pub fn label(&self) -> &'static str {
        match self.kind {
            SourceAnnotationErrorKind::Duplicate { .. } => "duplicate source-level annotation",
            _ => "in this source-level annotation",
        }
    }
}

impl fmt::Display for SourceAnnotationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            SourceAnnotationErrorKind::Duplicate { name, .. } => {
                write!(f, "this source already has a '@[{name}]' annotation")
            }
            SourceAnnotationErrorKind::Unknown { name } => {
                write!(f, "unknown annotation '{name}'")
            }
            SourceAnnotationErrorKind::NotSourceLevel { name } => {
                write!(f, "'@{name}' is not a source-level annotation")
            }
            SourceAnnotationErrorKind::InvalidArguments { name, reason } => {
                write!(f, "invalid arguments to '@[{name}]': {reason}")
            }
        }
    }
}

pub fn source_condition(
    annotations: &[AnnotationNode],
) -> Result<Option<&AnnotationNode>, SourceAnnotationError> {
    let mut conditions = annotations
        .iter()
        .filter(|annotation| annotation.name.as_ref() == CONDITION);
    let first = conditions.next();
    if let Some(second) = conditions.next() {
        return Err(SourceAnnotationError::new(
            second,
            SourceAnnotationErrorKind::Duplicate {
                name: second.name.clone(),
                first: first.unwrap().span,
            },
        ));
    }
    if let Some(annotation) = first
        && !matches!(annotation.args.as_slice(), [AnnotationArg::Positional(_)])
    {
        return Err(SourceAnnotationError::new(
            annotation,
            SourceAnnotationErrorKind::InvalidArguments {
                name: annotation.name.clone(),
                reason: "expected exactly one positional condition".into(),
            },
        ));
    }
    Ok(first)
}

pub fn resolve(annotations: &[AnnotationNode]) -> (SourceAnnotations, Vec<SourceAnnotationError>) {
    let mut result = SourceAnnotations::default();
    let mut errors = Vec::new();
    let mut seen: Vec<&AnnotationNode> = Vec::new();
    for annotation in annotations {
        let name = annotation.name.as_ref();
        if name == CONDITION {
            continue;
        }
        let kind = if let Some(first) = seen.iter().find(|first| first.name == annotation.name) {
            Some(SourceAnnotationErrorKind::Duplicate {
                name: annotation.name.clone(),
                first: first.span,
            })
        } else if !RECOGNIZED_NAMES.contains(&name) {
            Some(SourceAnnotationErrorKind::Unknown {
                name: annotation.name.clone(),
            })
        } else if name != SUPPRESS {
            Some(SourceAnnotationErrorKind::NotSourceLevel {
                name: annotation.name.clone(),
            })
        } else {
            None
        };
        seen.push(annotation);
        if let Some(kind) = kind {
            errors.push(SourceAnnotationError::new(annotation, kind));
            continue;
        }
        for arg in &annotation.args {
            match arg.bare_name() {
                Some(name) => result.suppress.push(name.clone()),
                None => errors.push(SourceAnnotationError::new(
                    annotation,
                    SourceAnnotationErrorKind::InvalidArguments {
                        name: annotation.name.clone(),
                        reason: "expected a bare warning name".into(),
                    },
                )),
            }
        }
    }
    (result, errors)
}

#[cfg(test)]
mod tests;
