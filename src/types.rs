use super::*;

#[derive(Eq, Hash, PartialEq, Clone)]
pub enum ObjectFieldType {
    ControlBlock,
    LambdaFunction,
    SubObject,
    Int,
    Bool,
}

impl ObjectFieldType {
    pub fn to_basic_type<'ctx>(&self, context: &'ctx Context) -> BasicTypeEnum<'ctx> {
        match self {
            ObjectFieldType::ControlBlock => control_block_type(context).into(),
            ObjectFieldType::LambdaFunction => ptr_to_lambda_function_type(context).into(),
            ObjectFieldType::SubObject => ptr_to_object_type(context).into(),
            ObjectFieldType::Int => context.i64_type().into(),
            ObjectFieldType::Bool => context.i8_type().into(),
        }
    }
}

#[derive(Eq, Hash, PartialEq, Clone)]
pub struct ObjectType {
    pub field_types: Vec<ObjectFieldType>,
}

impl ObjectType {
    pub fn to_struct_type<'ctx>(&self, context: &'ctx Context) -> StructType<'ctx> {
        let mut fields: Vec<BasicTypeEnum<'ctx>> = vec![];
        for field_type in &self.field_types {
            fields.push(field_type.to_basic_type(context));
        }
        context.struct_type(&fields, false)
    }

    fn shared_obj_type(mut field_types: Vec<ObjectFieldType>) -> Self {
        let mut fields = vec![ObjectFieldType::ControlBlock];
        fields.append(&mut field_types);
        Self {
            field_types: fields,
        }
    }

    pub fn int_obj_type() -> Self {
        Self::shared_obj_type(vec![ObjectFieldType::Int])
    }

    pub fn bool_obj_type() -> Self {
        Self::shared_obj_type(vec![ObjectFieldType::Bool])
    }

    pub fn lam_obj_type() -> Self {
        let mut field_types: Vec<ObjectFieldType> = Default::default();
        field_types.push(ObjectFieldType::ControlBlock);
        field_types.push(ObjectFieldType::LambdaFunction);
        // Following fields may exist, but their types are unknown.
        ObjectType { field_types }
    }

    fn generate_func_dtor<'c, 'm>(&self, gc: &mut GenerationContext<'c, 'm>) -> FunctionValue<'c> {
        if gc
            .runtimes
            .contains_key(&RuntimeFunctions::Dtor(self.clone()))
        {
            return *gc
                .runtimes
                .get(&RuntimeFunctions::Dtor(self.clone()))
                .unwrap();
        }
        let struct_type = self.to_struct_type(gc.context);
        let func_type = dtor_type(gc.context);
        let func = gc.module.add_function("dtor", func_type, None);
        let bb = gc.context.append_basic_block(func, "entry");

        let builder_guard = gc.push_builder();

        let context = gc.context;
        let module = gc.module;

        gc.builder().position_at_end(bb);
        let ptr_to_obj = func.get_first_param().unwrap().into_pointer_value();
        for (i, ft) in self.field_types.iter().enumerate() {
            match ft {
                ObjectFieldType::SubObject => {
                    let ptr_to_subobj = gc
                        .build_load_field_of_obj(ptr_to_obj, struct_type, i as u32)
                        .into_pointer_value();
                    gc.build_release(ptr_to_subobj);
                }
                ObjectFieldType::ControlBlock => {}
                ObjectFieldType::Int => {}
                ObjectFieldType::LambdaFunction => {}
                ObjectFieldType::Bool => {}
            }
        }
        gc.builder().build_return(None);

        // gc.pop_builder();
        gc.runtimes
            .insert(RuntimeFunctions::Dtor(self.clone()), func);
        func
    }

    pub fn build_allocate_shared_obj<'c, 'm>(
        &self,
        gc: &mut GenerationContext<'c, 'm>,
        name: Option<&str>,
    ) -> PointerValue<'c> {
        let context = gc.context;
        let struct_type = self.to_struct_type(context);
        // NOTE: Only once allocation is needed since we don't implement weak_ptr
        let ptr_to_obj = gc
            .builder()
            .build_malloc(struct_type, "ptr_to_obj")
            .unwrap();

        let mut object_id = obj_id_type(gc.context).const_int(0, false);

        if SANITIZE_MEMORY {
            let string_ptr = name.unwrap_or("N/A");
            let string_ptr = gc
                .builder()
                .build_global_string_ptr(string_ptr, "name_of_obj");
            let string_ptr = string_ptr.as_pointer_value();
            let string_ptr = gc.builder().build_pointer_cast(
                string_ptr,
                gc.context.i8_type().ptr_type(AddressSpace::Generic),
                "name_of_obj_i8ptr",
            );
            let ptr = gc.build_pointer_cast(ptr_to_obj, ptr_to_object_type(gc.context));
            let obj_id = gc.call_runtime(
                RuntimeFunctions::ReportMalloc,
                &[ptr.into(), string_ptr.into()],
            );
            object_id = obj_id.try_as_basic_value().unwrap_left().into_int_value();
        }

        for (i, ft) in self.field_types.iter().enumerate() {
            match ft {
                ObjectFieldType::ControlBlock => {
                    let ptr_to_control_block = gc
                        .builder()
                        .build_struct_gep(ptr_to_obj, i as u32, "ptr_to_control_block")
                        .unwrap();
                    let ptr_to_refcnt = gc
                        .builder()
                        .build_struct_gep(ptr_to_control_block, 0, "ptr_to_refcnt")
                        .unwrap();
                    // The initial value of refcnt should be one (as std::make_shared of C++ does).
                    gc.builder()
                        .build_store(ptr_to_refcnt, refcnt_type(context).const_int(1, false));
                    let ptr_to_dtor_field = gc
                        .builder()
                        .build_struct_gep(ptr_to_control_block, 1, "ptr_to_dtor_field")
                        .unwrap();
                    let dtor = self.generate_func_dtor(gc);
                    gc.builder()
                        .build_store(ptr_to_dtor_field, dtor.as_global_value().as_pointer_value());

                    if SANITIZE_MEMORY {
                        let ptr_to_obj_id = gc
                            .builder()
                            .build_struct_gep(ptr_to_control_block, 2, "ptr_to_obj_id")
                            .unwrap();
                        gc.builder().build_store(ptr_to_obj_id, object_id);
                    }
                }
                ObjectFieldType::Int => {}
                ObjectFieldType::SubObject => {}
                ObjectFieldType::LambdaFunction => {}
                ObjectFieldType::Bool => {}
            }
        }
        ptr_to_obj
    }
}

pub fn refcnt_type<'ctx>(context: &'ctx Context) -> IntType<'ctx> {
    context.i64_type()
}

fn ptr_to_refcnt_type<'ctx>(context: &'ctx Context) -> PointerType<'ctx> {
    refcnt_type(context).ptr_type(AddressSpace::Generic)
}

pub fn obj_id_type<'ctx>(context: &'ctx Context) -> IntType<'ctx> {
    context.i64_type()
}

pub fn ptr_to_object_type<'ctx>(context: &'ctx Context) -> PointerType<'ctx> {
    context.i8_type().ptr_type(AddressSpace::Generic)
}

fn dtor_type<'ctx>(context: &'ctx Context) -> FunctionType<'ctx> {
    context
        .void_type()
        .fn_type(&[ptr_to_object_type(context).into()], false)
}

fn ptr_to_dtor_type<'ctx>(context: &'ctx Context) -> PointerType<'ctx> {
    dtor_type(context).ptr_type(AddressSpace::Generic)
}

pub fn control_block_type<'ctx>(context: &'ctx Context) -> StructType<'ctx> {
    let mut fields = vec![
        refcnt_type(context).into(),
        ptr_to_dtor_type(context).into(),
    ];
    if SANITIZE_MEMORY {
        fields.push(obj_id_type(context).into())
    }
    context.struct_type(&fields, false)
}

pub fn ptr_to_control_block_type<'ctx>(context: &'ctx Context) -> PointerType<'ctx> {
    control_block_type(context).ptr_type(AddressSpace::Generic)
}

pub fn lambda_function_type<'ctx>(context: &'ctx Context) -> FunctionType<'ctx> {
    // A function that takes argument and context (=lambda object itself).
    ptr_to_object_type(context).fn_type(
        &[
            ptr_to_object_type(context).into(),
            ptr_to_object_type(context).into(),
        ],
        false,
    )
}

fn ptr_to_lambda_function_type<'ctx>(context: &'ctx Context) -> PointerType<'ctx> {
    lambda_function_type(context).ptr_type(AddressSpace::Generic)
}
