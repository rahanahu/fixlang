use super::*;

fn execute_main_module<'c>(
    context: &'c Context,
    module: &Module<'c>,
    opt_level: OptimizationLevel,
) -> i64 {
    if SANITIZE_MEMORY {
        assert_eq!(
            load_library_permanently("sanitizer/libfixsanitizer.so"),
            false
        );
    }
    let execution_engine = module.create_jit_execution_engine(opt_level).unwrap();
    unsafe {
        let func = execution_engine
            .get_function::<unsafe extern "C" fn() -> i64>("main")
            .unwrap();
        func.call()
    }
}

fn run_ast(program: Arc<ExprInfo>, opt_level: OptimizationLevel) -> i64 {
    // Add library functions to program.
    let program = let_in(var_var("add"), add(), program);
    let program = let_in(var_var("eq"), eq(), program);
    let program = let_in(var_var("fix"), fix(), program);
    let program = let_in(var_var("newArray"), new_array(), program);
    let program = let_in(var_var("readArray"), read_array(), program);
    let program = let_in(var_var("writeArray"), write_array(), program);
    let program = let_in(var_var("writeArray!"), write_array_unique(), program);

    // Calculate free variables of nodes.
    let program = calculate_free_vars(program);

    // Create GenerationContext.
    let context = Context::create();
    let module = context.create_module("main");
    let mut gc = GenerationContext::new(&context, &module);

    // Build runtime functions.
    build_runtime(&mut gc);

    // Add main function.
    let main_fn_type = context.i64_type().fn_type(&[], false);
    let main_function = module.add_function("main", main_fn_type, None);
    let entry_bb = context.append_basic_block(main_function, "entry");
    gc.builder().position_at_end(entry_bb);

    // Evaluate program and extract int value from result.
    let program_result = gc.eval_expr(program);
    let result = gc.load_obj_field(program_result, int_type(&context), 1);
    gc.release(program_result);

    // Perform leak check
    if SANITIZE_MEMORY {
        gc.call_runtime(RuntimeFunctions::CheckLeak, &[]);
    }

    // Build return
    if let BasicValueEnum::IntValue(result) = result {
        gc.builder().build_return(Some(&result));
    } else {
        panic!("Given program doesn't return int value!");
    }

    // Print LLVM bitcode to file
    module.print_to_file("main.ll").unwrap();

    // Verify LLVM module.
    let verify = module.verify();
    if verify.is_err() {
        print!("{}", verify.unwrap_err().to_str().unwrap());
        panic!("LLVM verify failed!");
    }

    // Run the module.
    execute_main_module(&context, &module, opt_level)
}

pub fn run_source(source: &str, opt_level: OptimizationLevel) -> i64 {
    let ast = parse_source(source);
    run_ast(ast, opt_level)
}

pub fn run_file(path: &Path, opt_level: OptimizationLevel) -> i64 {
    let display = path.display();

    let mut file = match File::open(&path) {
        Err(why) => panic!("Couldn't open {}: {}", display, why),
        Ok(file) => file,
    };

    // Read the file contents into a string, returns `io::Result<usize>`
    // ファイルの中身を文字列に読み込む。`io::Result<useize>`を返す。
    let mut s = String::new();
    match file.read_to_string(&mut s) {
        Err(why) => panic!("Couldn't read {}: {}", display, why),
        Ok(_) => (),
    }

    run_source(s.as_str(), opt_level)
}
