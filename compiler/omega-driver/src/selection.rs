use crate::modules::LoadFailure;
use crate::{CompileError, Driver, ModulePath};
use omega_analyzer::{annotation_eval, source_annotations};

impl Driver {
    pub(crate) fn select_modules(&mut self) -> Result<(), Vec<CompileError>> {
        let had_local = !self.roots.local_sources().is_empty();
        let mut disabled: Vec<ModulePath> = Vec::new();
        for path in self.roots.source_modules() {
            if disabled.iter().any(|parent| path.starts_with(parent)) {
                continue;
            }
            let Ok(location) = self.roots.locate(&path) else {
                continue;
            };
            let Some(file) = location.own_file else {
                continue;
            };
            let Ok(ast) = self.ensure_raw_ast(&path, &file) else {
                continue;
            };
            let condition = match source_annotations::source_condition(&ast.annotations) {
                Ok(condition) => condition,
                Err(error) => {
                    self.source_selection_failure(
                        &path,
                        LoadFailure::Compile(CompileError::SourceAnnotation {
                            module: path.clone(),
                            error,
                        }),
                    );
                    continue;
                }
            };
            if let Some(condition) = condition {
                match annotation_eval::evaluate(&self.definitions, condition) {
                    Ok(false) => {
                        disabled.push(path);
                        continue;
                    }
                    Ok(true) => {}
                    Err(error) => {
                        self.source_selection_failure(&path, LoadFailure::Condition(error));
                        continue;
                    }
                }
            }
            let (annotations, errors) = source_annotations::resolve(&ast.annotations);
            if let Some(error) = errors.into_iter().next() {
                self.source_selection_failure(
                    &path,
                    LoadFailure::Compile(CompileError::SourceAnnotation {
                        module: path.clone(),
                        error,
                    }),
                );
            } else {
                self.modules.source_annotations.insert(path, annotations);
            }
        }
        self.roots.prune(&disabled);
        // A surviving discovery error is a better explanation of an empty
        // package than the conditions are, and `local_module_paths` still
        // reports it, so claim "everything was conditioned out" only when
        // conditions are the whole story.
        if had_local
            && self.roots.local_sources().is_empty()
            && self
                .roots
                .local_modules()
                .all(|(_, location)| location.is_ok())
        {
            return Err(vec![CompileError::FullyDisabledPackage {
                root: self.roots.local_root().0,
            }]);
        }
        Ok(())
    }
}
