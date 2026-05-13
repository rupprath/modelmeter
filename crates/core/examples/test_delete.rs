/// Simulates the exact async spawn_blocking path used by the remove_provider
/// Tauri command, against the real production DB.
/// Run with: cargo run --example test_delete -p modelmeter-core

use modelmeter_core::{crud, db::Database};

#[tokio::main]
async fn main() {
    let db = Database::open().expect("open production DB");

    // Read current providers (same path as list_providers command).
    let before = {
        let db = db.clone();
        tokio::task::spawn_blocking(move || db.with_conn(crud::list_providers))
            .await.unwrap().unwrap()
    };
    println!("Providers before delete:");
    for p in &before {
        println!("  id={}, display_name={:?}", p.id, p.display_name);
    }

    if before.is_empty() {
        println!("No providers to delete.");
        return;
    }

    let target = before[0].id;
    println!("\nAttempting delete of id={target} via spawn_blocking...");

    let db2 = db.clone();
    let deleted = tokio::task::spawn_blocking(move || {
        db2.with_conn(move |c| crud::delete_provider(c, target))
    })
    .await.unwrap().unwrap();

    println!("delete_provider returned: {deleted}");

    // Read again (same path as list_providers after delete).
    let after = {
        let db = db.clone();
        tokio::task::spawn_blocking(move || db.with_conn(crud::list_providers))
            .await.unwrap().unwrap()
    };
    println!("\nProviders after delete:");
    for p in &after {
        println!("  id={}, display_name={:?}", p.id, p.display_name);
    }
    if after.is_empty() {
        println!("  (none)");
    }

    assert!(deleted, "expected delete to return true");
    assert_eq!(after.len(), before.len() - 1, "provider count should have decreased by 1");
    println!("\nAll checks passed — async delete path works against production DB.");
}
