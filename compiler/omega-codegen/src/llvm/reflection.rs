use super::Codegen;
use super::constant::ConstBlob;
use inkwell::module::Linkage;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, GlobalValue};
use omega_analyzer::layout::{self, EnumView};
use omega_analyzer::resolved_type::{ConstValue, Reflected, ResolvedType};

impl<'ctx> Codegen<'ctx> {
    pub(super) fn reflected_global(&mut self, reflected: &Reflected) -> GlobalValue<'ctx> {
        match reflected {
            Reflected::TypeInfo { described, table } => self.typeinfo_global(described, table),
            Reflected::VariantPrototype { r#type, variant } => {
                self.variant_prototype_global(r#type, *variant)
            }
        }
    }

    fn typeinfo_global(
        &mut self,
        described: &ResolvedType,
        info_type: &ResolvedType,
    ) -> GlobalValue<'ctx> {
        let symbol =
            omega_mir::mangle::encode(&omega_mir::mangle::typeinfo_symbol(&described.widened()));
        if let Some(&global) = self.reflected.get(&symbol) {
            return global;
        }

        // A table can reach itself through the tables it points to, but its
        // LLVM type is only known once its initializer is built. References
        // made meanwhile go to a placeholder, which is materialized into IR
        // before being replaced here.
        let placeholder = self.module.add_global(
            self.context.i8_type(),
            None,
            &format!("{symbol}.placeholder"),
        );
        self.reflected.insert(symbol.clone(), placeholder);

        let value =
            omega_analyzer::reflection::type_info_value(described, info_type, self.pointer_bytes());
        let blob = self.build_const_blob(&value, info_type);
        let (ty, init) = self.materialize_blob(&blob);
        let global = self.declare_reflected(&symbol, ty, init, layout::type_alignment(info_type));

        placeholder
            .as_pointer_value()
            .replace_all_uses_with(global.as_pointer_value());
        // SAFETY: every use was just replaced, and the placeholder handle is
        // dropped from the cache below.
        unsafe { placeholder.delete() };
        self.reflected.insert(symbol, global);
        global
    }

    fn variant_prototype_global(
        &mut self,
        r#type: &ResolvedType,
        variant: usize,
    ) -> GlobalValue<'ctx> {
        let symbol = omega_mir::mangle::encode(&omega_mir::mangle::variant_prototype_symbol(
            r#type, variant,
        ));
        if let Some(&global) = self.reflected.get(&symbol) {
            return global;
        }

        let pointer_bytes = self.pointer_bytes();
        let view = EnumView::of(r#type).expect("a variant prototype describes an enum-like type");
        let facts = &view.variants[variant];
        let mut blob = ConstBlob::zeroed(layout::total_bytes(r#type, pointer_bytes), pointer_bytes);
        self.write_const_element(&mut blob, 0, &ConstValue::Number(facts.tag), &view.tag_type);
        for (index, (field_type, value)) in view.header.iter().zip(&facts.header_values).enumerate()
        {
            let offset = layout::enum_header_offset(&view, index, pointer_bytes);
            self.write_const_element(&mut blob, offset, value, field_type);
        }
        let (ty, init) = self.materialize_blob(&blob);
        let global = self.declare_reflected(&symbol, ty, init, layout::type_alignment(r#type));
        self.reflected.insert(symbol, global);
        global
    }

    fn declare_reflected(
        &mut self,
        symbol: &str,
        ty: BasicTypeEnum<'ctx>,
        init: BasicValueEnum<'ctx>,
        align: u32,
    ) -> GlobalValue<'ctx> {
        let global = self.module.add_global(ty, None, symbol);
        global.set_linkage(Linkage::WeakODR);
        global.set_visibility(inkwell::GlobalVisibility::Hidden);
        global.set_initializer(&init);
        global.set_constant(true);
        global.set_alignment(align);
        if self.target.os != omega_analyzer::Os::MacOs {
            global.set_section(Some(&format!(".rodata.{symbol}")));
        }
        global
    }
}
