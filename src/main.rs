use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    if let Some(output) = workstate::meta_output(arguments.clone()) {
        print!("{output}");
        return ExitCode::SUCCESS;
    }

    let context = match workstate::AppContext::bootstrap() {
        Ok(context) => context,
        Err(error) => return report_error(error, &arguments),
    };

    match workstate::run_with_args(context, arguments.clone()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => report_error(error, &arguments),
    }
}

fn report_error(error: workstate::WorkstateError, arguments: &[std::ffi::OsString]) -> ExitCode {
    eprintln!("{}", workstate::render_error_for_args(arguments, &error));
    ExitCode::from(error.exit_code())
}
