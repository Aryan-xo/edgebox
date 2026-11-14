fn main() {
    println!("Hello from WASM!");

    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        println!("args: {args:?}");
    }
}
