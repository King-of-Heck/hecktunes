pub mod artwork;
pub mod identify;
pub mod import;
pub mod library;
pub mod model;
pub mod tags;
#[cfg(test)]
mod test_support;

pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running HeckTunes");
}
