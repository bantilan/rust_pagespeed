//! Diagnostic for the legacy shared-counter creator/attached-reader lifetime.
//! This does not mutate live cache files: it always uses its own temporary path.
use ps_shm::SharedCounters;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!(
        "ps-counter-lifetime-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir(&directory)?;
    let names = vec!["generation".to_string()];
    let open = if std::env::args().any(|arg| arg == "--coherent") {
        SharedCounters::open_coherent
    } else {
        SharedCounters::open
    };
    let creator = open(&directory, &names)?;
    let surviving_reader = open(&directory, &names)?;
    creator.add(0, 1);
    let before = surviving_reader.get(0);
    drop(creator);
    let new_opener = open(&directory, &names)?;
    let after = surviving_reader.get(0);
    println!("surviving_reader_before={before}");
    println!("surviving_reader_after={after}");
    println!("generation_reset_while_reader_alive={}", before != after);
    drop(surviving_reader);
    drop(new_opener);
    std::fs::remove_dir_all(directory)?;
    Ok(())
}
