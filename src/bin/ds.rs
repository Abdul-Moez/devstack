//! `ds.exe`: the devstack command line.

fn main() {
    std::process::exit(devstack::cli::run(std::env::args().collect()));
}
