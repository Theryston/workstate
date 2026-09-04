use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let context = match workstate::AppContext::bootstrap() {
        Ok(context) => context,
        Err(error) => return report_error(error),
    };

    match workstate::run(context).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => report_error(error),
    }
}

fn report_error(error: workstate::WorkstateError) -> ExitCode {
    eprintln!("{}", error.render());
    ExitCode::from(error.exit_code())
}
