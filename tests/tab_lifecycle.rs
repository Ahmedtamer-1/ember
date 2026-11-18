use std::time::{Duration, Instant};

use ember::tabs::{CloseTabOutcome, TabManager};

#[test]
fn test_integration_tab_creation_and_switching() {
    let mut manager = TabManager::new(Duration::from_secs(300));
    assert_eq!(manager.active_tab_id(), 0);

    // Initial tab
    let tab1 = manager.create_tab("https://example.com", true);
    assert_eq!(manager.active_tab_id(), tab1);
    assert_eq!(manager.summaries().len(), 1);

    // Second tab created in background
    let tab2 = manager.create_tab("https://rust-lang.org", false);
    assert_eq!(manager.active_tab_id(), tab1);
    assert_eq!(manager.summaries().len(), 2);

    // Switch to tab 2
    assert!(manager.switch_to(tab2));
    assert_eq!(manager.active_tab_id(), tab2);

    // Switch next / prev
    assert!(manager.switch_next_tab());
    assert_eq!(manager.active_tab_id(), tab1);

    assert!(manager.switch_prev_tab());
    assert_eq!(manager.active_tab_id(), tab2);

    // Shortcut index (1-based)
    assert!(manager.switch_to_shortcut_number(1));
    assert_eq!(manager.active_tab_id(), tab1);

    assert!(manager.switch_to_shortcut_number(2));
    assert_eq!(manager.active_tab_id(), tab2);
}

#[test]
fn test_integration_tab_duplication_and_reordering() {
    let mut manager = TabManager::new(Duration::from_secs(300));
    let t1 = manager.create_tab("https://doc.rust-lang.org", true);
    manager.update_title(t1, String::from("Rust Docs"));
    manager.update_scroll_position(t1, 150.0, 420.0);

    // Duplicate t1
    let t2 = manager.duplicate_tab(t1, true).expect("duplicate tab");
    assert_eq!(manager.active_tab_id(), t2);
    let dup = manager.get_tab(t2).expect("duplicated tab exists");
    assert_eq!(dup.url, "https://doc.rust-lang.org");
    assert_eq!(dup.title, "Rust Docs");
    assert_eq!(dup.scroll_position, (150.0, 420.0));

    // Tab order: [t1, t2]
    let t3 = manager.create_tab("https://news.ycombinator.com", false);
    // Tab order: [t1, t2, t3]
    let ids: Vec<usize> = manager.summaries().iter().map(|s| s.id).collect();
    assert_eq!(ids, vec![t1, t2, t3]);

    // Reorder: drag t3 before t1
    assert!(manager.reorder_tab(t3, t1));
    let new_ids: Vec<usize> = manager.summaries().iter().map(|s| s.id).collect();
    assert_eq!(new_ids, vec![t3, t1, t2]);
}

#[test]
fn test_integration_background_tab_suspension_lifecycle() {
    let timeout = Duration::from_secs(10);
    let mut manager = TabManager::new(timeout);

    let t1 = manager.create_tab("https://active.test", true);
    let t2 = manager.create_tab("https://idle.test", false);

    // Simulate time passing: 15 seconds later
    let now = Instant::now() + Duration::from_secs(15);
    let suspended = manager.suspend_inactive_tabs(now);

    // Active tab t1 should NOT be suspended; idle background tab t2 should be suspended
    assert_eq!(suspended, vec![t2]);
    assert!(!manager.get_tab(t1).unwrap().is_suspended);
    assert!(manager.get_tab(t2).unwrap().is_suspended);

    // Switching to suspended tab restores it
    assert!(manager.switch_to(t2));
    assert_eq!(manager.active_tab_id(), t2);
    assert!(!manager.get_tab(t2).unwrap().is_suspended);
}

#[test]
fn test_integration_recently_closed_stack_and_reopen() {
    let mut manager = TabManager::new(Duration::from_secs(300));
    let t1 = manager.create_tab("https://site1.org", true);
    manager.update_title(t1, String::from("Site 1"));
    let t2 = manager.create_tab("https://site2.org", true);
    manager.update_title(t2, String::from("Site 2"));
    let t3 = manager.create_tab("https://site3.org", true);
    manager.update_title(t3, String::from("Site 3"));

    // Close t3
    assert_eq!(manager.close_tab(t3), CloseTabOutcome::Closed);
    assert_eq!(manager.recently_closed().len(), 1);
    assert_eq!(manager.recently_closed()[0].url, "https://site3.org");

    // Close t2
    assert_eq!(manager.close_tab(t2), CloseTabOutcome::Closed);
    assert_eq!(manager.recently_closed().len(), 2);
    assert_eq!(
        manager.recently_closed().last().unwrap().url,
        "https://site2.org"
    );

    // Reopen closed tab (LIFO: t2 reopened first)
    let reopened1 = manager.reopen_closed_tab().expect("reopen t2");
    assert_eq!(manager.get_tab(reopened1).unwrap().url, "https://site2.org");
    assert_eq!(manager.recently_closed().len(), 1);

    // Reopen closed tab (t3 reopened second)
    let reopened2 = manager.reopen_closed_tab().expect("reopen t3");
    assert_eq!(manager.get_tab(reopened2).unwrap().url, "https://site3.org");
    assert_eq!(manager.recently_closed().len(), 0);

    // Close until last tab
    assert_eq!(manager.close_tab(reopened2), CloseTabOutcome::Closed);
    assert_eq!(manager.close_tab(reopened1), CloseTabOutcome::Closed);
    assert_eq!(manager.close_tab(t1), CloseTabOutcome::LastTabClosed);
}

#[test]
fn test_integration_close_unknown_tab_not_found() {
    let mut manager = TabManager::new(Duration::from_secs(300));
    let t1 = manager.create_tab("https://site1.org", true);
    assert_eq!(manager.close_tab(t1 + 42), CloseTabOutcome::NotFound);
    assert_eq!(manager.summaries().len(), 1);
    assert!(manager.recently_closed().is_empty());
}

#[test]
fn test_integration_never_suspend_setting_does_not_suspend() {
    let settings = ember::settings::BrowserSettings {
        tab_suspend_minutes: 0,
        ..Default::default()
    };
    let timeout = settings.tab_suspend_timeout();
    assert!(timeout >= Duration::from_secs(365 * 24 * 3600));

    let mut manager = TabManager::new(timeout);
    let _active = manager.create_tab("https://active.test", true);
    let _bg = manager.create_tab("https://idle.test", false);
    let suspended = manager.suspend_inactive_tabs(Instant::now() + Duration::from_secs(3600));
    assert!(suspended.is_empty());
}

#[test]
fn test_integration_popup_blocker_requires_activation() {
    let mut manager = TabManager::new(Duration::from_secs(300));
    let t1 = manager.create_tab("https://site1.org", true);
    let now = Instant::now();
    assert!(!manager.try_consume_popup(t1, now));
    manager.note_user_activation(t1, now);
    assert!(manager.try_consume_popup(t1, now + Duration::from_millis(100)));
    assert!(!manager.try_consume_popup(t1, now + Duration::from_millis(200)));
    manager.note_user_activation(t1, now + Duration::from_secs(5));
    assert!(!manager.try_consume_popup(t1, now + Duration::from_secs(7)));
}
