pub mod model;
#[cfg(test)]
mod test_support;

pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running HeckTunes");
}
