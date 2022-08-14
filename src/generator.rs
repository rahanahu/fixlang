// generator module
// --
// GenerationContext struct, code generation and convenient functions.

use inkwell::values::{BasicMetadataValueEnum, CallSiteValue};

use super::*;

#[derive(Clone)]
pub struct ExprCode<'ctx> {
    pub ptr: PointerValue<'ctx>,
}

#[derive(Clone)]
pub struct LocalVariable<'ctx> {
    pub code: ExprCode<'ctx>,
    used_later: u32,
}

#[derive(Default)]
pub struct LocalVariables<'ctx> {
    data: HashMap<String, Vec<LocalVariable<'ctx>>>,
}

impl<'c> LocalVariables<'c> {
    fn push(self: &mut Self, var_name: &str, code: &ExprCode<'c>) {
        if !self.data.contains_key(var_name) {
            self.data.insert(String::from(var_name), Default::default());
        }
        self.data.get_mut(var_name).unwrap().push(LocalVariable {
            code: code.clone(),
            used_later: 0,
        });
    }
    fn pop(self: &mut Self, var_name: &str) {
        self.data.get_mut(var_name).unwrap().pop();
        if self.data.get(var_name).unwrap().is_empty() {
            self.data.remove(var_name);
        }
    }
    pub fn get(self: &Self, var_name: &str) -> LocalVariable<'c> {
        self.data.get(var_name).unwrap().last().unwrap().clone()
    }
    pub fn get_field<'m, 'b>(
        self: &Self,
        var_name: &str,
        field_idx: u32,
        ty: StructType<'c>,
        gc: &GenerationContext<'c, 'm, 'b>,
    ) -> BasicValueEnum<'c> {
        let expr = self.get(var_name);
        gc.build_load_field_of_obj(expr.code.ptr, ty, field_idx)
    }
    fn modify_used_later(self: &mut Self, names: &HashSet<String>, by: i32) {
        for name in names {
            let used_later = &mut self
                .data
                .get_mut(name)
                .unwrap()
                .last_mut()
                .unwrap()
                .used_later;
            *used_later = add_i32_to_u32(*used_later, by);
        }
    }
    fn increment_used_later(self: &mut Self, names: &HashSet<String>) {
        self.modify_used_later(names, 1);
    }
    fn decrement_used_later(self: &mut Self, names: &HashSet<String>) {
        self.modify_used_later(names, -1);
    }
}

fn add_i32_to_u32(u: u32, i: i32) -> u32 {
    if i.is_negative() {
        u - i.wrapping_abs() as u32
    } else {
        u + i as u32
    }
}

pub struct GenerationContext<'c, 'm, 'b> {
    pub context: &'c Context,
    pub module: &'m Module<'c>,
    pub builder: &'b Builder<'c>,
    pub scope: LocalVariables<'c>,
    pub runtimes: HashMap<RuntimeFunctions, FunctionValue<'c>>,
}

impl<'c, 'm, 'b> GenerationContext<'c, 'm, 'b> {
    pub fn push_builder<'s: 'd, 'd>(
        self: &'s mut Self,
        builder: &'d Builder<'c>,
    ) -> (
        GenerationContext<'c, 'm, 'd>,
        impl 's + FnOnce(GenerationContext<'c, 'm, 'd>),
    ) {
        let new_gc = GenerationContext {
            context: self.context,
            module: self.module,
            builder,
            scope: std::mem::replace(&mut self.scope, Default::default()),
            runtimes: std::mem::replace(&mut self.runtimes, Default::default()),
        };
        let pop = |gc: GenerationContext<'c, 'm, 'd>| {
            self.scope = gc.scope;
            self.runtimes = gc.runtimes;
        };
        (new_gc, pop)
    }
    pub fn get_var_retained_if_used_later(&mut self, var_name: &str) -> ExprCode<'c> {
        let var = self.scope.get(var_name);
        let code = var.code;
        if var.used_later > 0 {
            self.build_retain(code.ptr);
        }
        code
    }
    pub fn build_pointer_cast(
        &self,
        from: PointerValue<'c>,
        to: PointerType<'c>,
    ) -> PointerValue<'c> {
        if from.get_type() == to {
            from
        } else {
            self.builder.build_pointer_cast(from, to, "pointer_cast")
        }
    }

    // Get pointer to control block of a given object.
    pub fn build_ptr_to_control_block(&self, obj: PointerValue<'c>) -> PointerValue<'c> {
        self.build_pointer_cast(obj, ptr_to_control_block_type(self.context))
    }

    // Get pointer to reference counter of a given object.
    pub fn build_ptr_to_refcnt(&self, obj: PointerValue<'c>) -> PointerValue<'c> {
        let ptr_control_block = self.build_ptr_to_control_block(obj);
        self.builder
            .build_struct_gep(ptr_control_block, 0, "ptr_to_refcnt")
            .unwrap()
    }

    // Call dtor of object.
    pub fn build_call_dtor(&self, obj: PointerValue<'c>) {
        let ptr_to_dtor = self
            .build_load_field_of_obj(obj, control_block_type(self.context), 1)
            .into_pointer_value();
        let dtor_func = CallableValue::try_from(ptr_to_dtor).unwrap();
        self.builder
            .build_call(dtor_func, &[obj.into()], "call_dtor");
    }

    // Take an pointer of struct and return the loaded value of a pointer field.
    pub fn build_load_field_of_obj(
        &self,
        obj: PointerValue<'c>,
        ty: StructType<'c>,
        index: u32,
    ) -> BasicValueEnum<'c> {
        let ptr = self.build_pointer_cast(obj, ptr_type(ty));
        let ptr_to_field = self
            .builder
            .build_struct_gep(ptr, index, "ptr_to_field")
            .unwrap();
        self.builder.build_load(ptr_to_field, "field_value")
    }

    // Take an pointer of struct and store a value value into a pointer field.
    pub fn build_set_field<V>(
        &self,
        obj: PointerValue<'c>,
        ty: StructType<'c>,
        index: u32,
        value: V,
    ) where
        V: BasicValue<'c>,
    {
        let ptr = self.build_pointer_cast(obj, ptr_type(ty));
        let ptr_to_field = self
            .builder
            .build_struct_gep(ptr, index, "ptr_to_field")
            .unwrap();
        self.builder.build_store(ptr_to_field, value);
    }

    // Take a closure object and return function pointer.
    fn build_ptr_to_func_of_lambda(&self, obj: PointerValue<'c>) -> PointerValue<'c> {
        let lam_obj_ty = ObjectType::lam_obj_type().to_struct_type(self.context);
        self.build_load_field_of_obj(obj, lam_obj_ty, 1)
            .into_pointer_value()
    }

    // Apply a object to a closure.
    pub fn build_app(
        &self,
        ptr_to_lambda: PointerValue<'c>,
        ptr_to_arg: PointerValue<'c>,
    ) -> ExprCode<'c> {
        let ptr_to_func = self.build_ptr_to_func_of_lambda(ptr_to_lambda);
        let lambda_func = CallableValue::try_from(ptr_to_func).unwrap();
        let ret = self.builder.build_call(
            lambda_func,
            &[ptr_to_arg.into(), ptr_to_lambda.into()],
            "call_lambda",
        );
        ret.set_tail_call(true);
        let ret = ret.try_as_basic_value().unwrap_left().into_pointer_value();
        ExprCode { ptr: ret }
    }

    // Retain object.
    fn build_retain(&self, ptr_to_obj: PointerValue<'c>) {
        if ptr_to_obj.get_type() != ptr_to_object_type(self.context) {
            panic!("type of arg of build_release is incorrect.");
        }
        self.call_runtime(RuntimeFunctions::RetainObj, &[ptr_to_obj.clone().into()]);
    }

    // Release object.
    pub fn build_release(&self, ptr_to_obj: PointerValue<'c>) {
        if ptr_to_obj.get_type() != ptr_to_object_type(self.context) {
            panic!("type of arg of build_release is incorrect.");
        }
        self.call_runtime(RuntimeFunctions::ReleaseObj, &[ptr_to_obj.clone().into()]);
    }

    // Get object id of a object
    pub fn build_get_obj_id(&self, ptr_to_obj: PointerValue<'c>) -> IntValue<'c> {
        assert!(SANITIZE_MEMORY);
        self.build_load_field_of_obj(ptr_to_obj, control_block_type(self.context), 2)
            .into_int_value()
    }

    // Call a runtime function.
    pub fn call_runtime(
        &self,
        func: RuntimeFunctions,
        args: &[BasicMetadataValueEnum<'c>],
    ) -> CallSiteValue<'c> {
        self.builder
            .build_call(*self.runtimes.get(&func).unwrap(), args, "call_runtime")
    }
}

pub fn ptr_type<'c>(ty: StructType<'c>) -> PointerType<'c> {
    ty.ptr_type(AddressSpace::Generic)
}

pub fn generate_expr<'c, 'm, 'b>(
    expr: Arc<ExprInfo>,
    gc: &mut GenerationContext<'c, 'm, 'b>,
) -> ExprCode<'c> {
    let mut ret = match &*expr.expr {
        Expr::Var(var) => generate_var(var.clone(), gc),
        Expr::Lit(lit) => generate_literal(lit.clone(), gc),
        Expr::App(lambda, arg) => generate_app(lambda.clone(), arg.clone(), gc),
        Expr::Lam(arg, val) => generate_lam(arg.clone(), val.clone(), gc),
        Expr::Let(var, bound, expr) => generate_let(var.clone(), bound.clone(), expr.clone(), gc),
        Expr::If(cond_expr, then_expr, else_expr) => {
            generate_if(cond_expr.clone(), then_expr.clone(), else_expr.clone(), gc)
        }
        Expr::Type(_) => todo!(),
    };
    ret.ptr = gc.build_pointer_cast(ret.ptr, ptr_to_object_type(gc.context));
    ret
}

fn generate_var<'c, 'm, 'b>(var: Arc<Var>, gc: &mut GenerationContext<'c, 'm, 'b>) -> ExprCode<'c> {
    match &*var {
        Var::TermVar { name } => gc.get_var_retained_if_used_later(name),
        Var::TyVar { name: _ } => unreachable!(),
    }
}

fn generate_app<'c, 'm, 'b>(
    lambda: Arc<ExprInfo>,
    arg: Arc<ExprInfo>,
    gc: &mut GenerationContext<'c, 'm, 'b>,
) -> ExprCode<'c> {
    gc.scope.increment_used_later(&arg.free_vars);
    let lambda_code = generate_expr(lambda, gc);
    gc.scope.decrement_used_later(&arg.free_vars);
    let arg_code = generate_expr(arg, gc);
    gc.build_app(lambda_code.ptr, arg_code.ptr)
    // We do not release arg.ptr and lambda.ptr here since we have moved them into the arguments of lambda_func.
}

fn generate_literal<'c, 'm, 'b>(
    lit: Arc<Literal>,
    gc: &mut GenerationContext<'c, 'm, 'b>,
) -> ExprCode<'c> {
    (lit.generator)(gc)
}

pub static SELF_NAME: &str = "%SELF%";

fn generate_lam<'c, 'm, 'b>(
    arg: Arc<Var>,
    val: Arc<ExprInfo>,
    gc: &mut GenerationContext<'c, 'm, 'b>,
) -> ExprCode<'c> {
    let context = gc.context;
    let module = gc.module;
    // Fix ordering of captured names
    let mut captured_names = val.free_vars.clone();
    captured_names.remove(arg.name());
    captured_names.remove(SELF_NAME);
    let captured_names: Vec<String> = captured_names.into_iter().collect();
    // Determine the type of closure
    let mut field_types = vec![
        ObjectFieldType::ControlBlock,
        ObjectFieldType::LambdaFunction,
    ];
    for _ in captured_names.iter() {
        field_types.push(ObjectFieldType::SubObject);
    }
    let obj_type = ObjectType { field_types };
    let closure_ty = obj_type.to_struct_type(context);
    // Declare lambda function
    let lam_fn_ty = lambda_function_type(context);
    let lam_fn = module.add_function("lambda", lam_fn_ty, None);
    // Implement lambda function
    {
        // Create new builder
        let builder = gc.context.create_builder();
        let bb = context.append_basic_block(lam_fn, "entry");
        builder.position_at_end(bb);
        // Create new gc
        let mut gc = GenerationContext {
            context,
            module,
            builder: &builder,
            scope: LocalVariables::default(),
            runtimes: gc.runtimes.clone(),
        };
        // Set up new scope
        let arg_ptr = lam_fn.get_first_param().unwrap().into_pointer_value();
        gc.scope.push(&arg.name(), &ExprCode { ptr: arg_ptr });
        let closure_obj = lam_fn.get_nth_param(1).unwrap().into_pointer_value();
        gc.scope.push(SELF_NAME, &ExprCode { ptr: closure_obj });
        for (i, cap_name) in captured_names.iter().enumerate() {
            let cap_obj = gc
                .build_load_field_of_obj(closure_obj, closure_ty, i as u32 + 2)
                .into_pointer_value();
            gc.scope.push(cap_name, &ExprCode { ptr: cap_obj });
        }
        // Retain captured objects
        for cap_name in &captured_names {
            let ptr = gc.scope.get(cap_name).code.ptr;
            gc.build_retain(ptr);
        }
        // Release SELF and arg if unused
        if !val.free_vars.contains(SELF_NAME) {
            gc.build_release(closure_obj);
        }
        if !val.free_vars.contains(arg.name()) {
            gc.build_release(arg_ptr);
        }
        // Generate value
        let val = generate_expr(val.clone(), &mut gc);
        // Return result
        let ptr = gc.build_pointer_cast(val.ptr, ptr_to_object_type(gc.context));
        builder.build_return(Some(&ptr));
    }
    // Allocate and set up closure
    let name = lam(arg, val).expr.to_string();
    let obj = obj_type.build_allocate_shared_obj(gc, Some(name.as_str()));
    gc.build_set_field(
        obj,
        closure_ty,
        1,
        lam_fn.as_global_value().as_pointer_value(),
    );
    for (i, cap) in captured_names.iter().enumerate() {
        let ptr = gc.get_var_retained_if_used_later(cap).ptr;
        gc.build_set_field(obj, closure_ty, i as u32 + 2, ptr);
    }
    // Return closure object
    ExprCode { ptr: obj }
}

fn generate_let<'c, 'm, 'b>(
    var: Arc<Var>,
    bound: Arc<ExprInfo>,
    val: Arc<ExprInfo>,
    gc: &mut GenerationContext<'c, 'm, 'b>,
) -> ExprCode<'c> {
    let var_name = var.name();
    let mut used_in_val_except_var = val.free_vars.clone();
    used_in_val_except_var.remove(var_name);
    gc.scope.increment_used_later(&used_in_val_except_var);
    let bound_code = generate_expr(bound.clone(), gc);
    gc.scope.decrement_used_later(&used_in_val_except_var);
    gc.scope.push(&var_name, &bound_code);
    if !val.free_vars.contains(var_name) {
        gc.build_release(bound_code.ptr);
    }
    let val_code = generate_expr(val.clone(), gc);
    gc.scope.pop(&var_name);
    val_code
}

fn generate_if<'c, 'm, 'b>(
    cond_expr: Arc<ExprInfo>,
    then_expr: Arc<ExprInfo>,
    else_expr: Arc<ExprInfo>,
    gc: &mut GenerationContext<'c, 'm, 'b>,
) -> ExprCode<'c> {
    let mut used_then_or_else = then_expr.free_vars.clone();
    used_then_or_else.extend(else_expr.free_vars.clone());
    gc.scope.increment_used_later(&used_then_or_else);
    let ptr_to_cond_obj = generate_expr(cond_expr, gc).ptr;
    gc.scope.decrement_used_later(&used_then_or_else);
    let bool_ty = ObjectType::bool_obj_type().to_struct_type(gc.context);
    let cond_val = gc
        .build_load_field_of_obj(ptr_to_cond_obj, bool_ty, 1)
        .into_int_value();
    gc.build_release(ptr_to_cond_obj);
    let cond_val = gc
        .builder
        .build_int_cast(cond_val, gc.context.bool_type(), "cond_val_i1");
    let bb = gc.builder.get_insert_block().unwrap();
    let func = bb.get_parent().unwrap();
    let then_bb = gc.context.append_basic_block(func, "then");
    let else_bb = gc.context.append_basic_block(func, "else");
    let cont_bb = gc.context.append_basic_block(func, "cont");
    gc.builder
        .build_conditional_branch(cond_val, then_bb, else_bb);

    gc.builder.position_at_end(then_bb);
    // Release variables used only in the else block.
    for var_name in &else_expr.free_vars {
        if !then_expr.free_vars.contains(var_name) && gc.scope.get(var_name).used_later == 0 {
            gc.build_release(gc.scope.get(var_name).code.ptr);
        }
    }
    let then_code = generate_expr(then_expr.clone(), gc);
    gc.builder.build_unconditional_branch(cont_bb);

    gc.builder.position_at_end(else_bb);
    // Release variables used only in the then block.
    for var_name in &then_expr.free_vars {
        if !else_expr.free_vars.contains(var_name) && gc.scope.get(var_name).used_later == 0 {
            gc.build_release(gc.scope.get(var_name).code.ptr);
        }
    }
    let else_code = generate_expr(else_expr, gc);
    gc.builder.build_unconditional_branch(cont_bb);

    gc.builder.position_at_end(cont_bb);
    let phi = gc.builder.build_phi(ptr_to_object_type(gc.context), "phi");
    phi.add_incoming(&[(&then_code.ptr, then_bb), (&else_code.ptr, else_bb)]);
    let ret_ptr = phi.as_basic_value().into_pointer_value();
    ExprCode { ptr: ret_ptr }
}
