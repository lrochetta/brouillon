//! `brouillon-docs`: stdout carries frames only; stderr is free text without document text.

fn main() {
    let code = brouillon_docs::serve(std::io::stdin(), std::io::stdout().lock());
    std::process::exit(code);
}
