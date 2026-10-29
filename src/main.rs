use clap::Parser;
use std::io::IsTerminal;

fn main() {
    let code = match rulescan::cli::Cli::try_parse() {
        Ok(args) => rulescan::cli::run(args),
        Err(error) => {
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                let _ = error.print();
                0
            } else {
                let args: Vec<_> = std::env::args_os().collect();
                let machine = !std::io::stdout().is_terminal()
                    || args.iter().any(|a| {
                        a == "json" || a == "jsonl" || a == "--format=json" || a == "--format=jsonl"
                    });
                if machine {
                    let value = serde_json::json!({"schema_version":1,"request_id":rulescan::model::new_id(),"operation":"invocation","completion":"failed","coverage":null,"profile_version":rulescan::model::PROFILE_VERSION,"policy_version":rulescan::model::POLICY_VERSION,"data":null,"errors":[rulescan::model::escape(&error.to_string(),false)]});
                    println!("{value}");
                } else {
                    let _ = error.print();
                }
                2
            }
        }
    };
    std::process::exit(code);
}
