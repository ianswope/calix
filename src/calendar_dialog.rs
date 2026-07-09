use crate::store::{Calendar, Store};
use adw::prelude::*;
use gtk::glib;
use gtk::glib::clone;
use std::rc::Rc;

pub fn build_list(store: Rc<Store>, on_changed: impl Fn() + 'static) -> gtk::Widget {
    let on_changed = Rc::new(on_changed);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_hexpand(true);
    content.set_vexpand(true);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    add_account_sections(
        &content,
        store.clone(),
        on_changed.clone(),
        "iCloud",
        "icloud",
    );
    add_account_sections(
        &content,
        store.clone(),
        on_changed.clone(),
        "Google",
        "google",
    );

    if let Ok(local_calendars) = store.local_calendars()
        && !local_calendars.is_empty()
    {
        content.append(&calendar_group(
            "On My Computer",
            None,
            local_calendars,
            store.clone(),
            on_changed.clone(),
        ));
    }

    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .hexpand(true)
        .vexpand(true)
        .child(&content)
        .build()
        .upcast()
}

fn add_account_sections(
    content: &gtk::Box,
    store: Rc<Store>,
    on_changed: Rc<dyn Fn()>,
    title: &str,
    provider: &str,
) {
    let accounts = match provider {
        "google" => store.google_accounts(),
        "icloud" => store.icloud_accounts(),
        _ => return,
    };

    match accounts {
        Ok(accounts) if accounts.is_empty() => {
            let empty_group = adw::PreferencesGroup::builder().title(title).build();
            let account_name = if provider == "icloud" {
                "iCloud"
            } else {
                "Google"
            };
            let row = adw::ActionRow::builder()
                .title(format!("No {account_name} accounts connected"))
                .subtitle(format!("Use Add {account_name} to connect an account"))
                .build();
            empty_group.add(&row);
            content.append(&empty_group);
        }
        Ok(accounts) => {
            let account_count = accounts.len();
            let calendar_count = accounts
                .iter()
                .filter_map(|account| store.calendars_for_account(account.id).ok())
                .map(|calendars| calendars.len())
                .sum::<usize>();
            let account_name = if provider == "icloud" {
                "iCloud"
            } else {
                "Google"
            };
            let summary_group = adw::PreferencesGroup::builder().title(title).build();
            summary_group.add(
                &adw::ActionRow::builder()
                    .title(format!(
                        "{calendar_count} calendar{} from {account_count} {account_name} account{}",
                        if calendar_count == 1 { "" } else { "s" },
                        if account_count == 1 { "" } else { "s" }
                    ))
                    .build(),
            );
            content.append(&summary_group);

            for account in accounts {
                let calendars = store.calendars_for_account(account.id).unwrap_or_default();
                content.append(&calendar_group(
                    "Account",
                    Some(&account.display_name),
                    calendars,
                    store.clone(),
                    on_changed.clone(),
                ));
            }
        }
        Err(_) => {
            let error_group = adw::PreferencesGroup::new();
            error_group.add(
                &adw::ActionRow::builder()
                    .title("Could not load calendars")
                    .build(),
            );
            content.append(&error_group);
        }
    }
}

fn calendar_group(
    title: &str,
    description: Option<&str>,
    calendars: Vec<Calendar>,
    store: Rc<Store>,
    on_changed: Rc<dyn Fn()>,
) -> gtk::Widget {
    let group = adw::PreferencesGroup::builder()
        .title(gtk::glib::markup_escape_text(title))
        .build();
    if let Some(description) = description {
        let escaped_description = gtk::glib::markup_escape_text(description);
        group.set_description(Some(escaped_description.as_str()));
    }

    if calendars.is_empty() {
        group.add(
            &adw::ActionRow::builder()
                .title("No calendars found")
                .subtitle("Sync this account to fetch its calendars")
                .build(),
        );
    } else {
        for calendar in calendars {
            group.add(&calendar_row(calendar, store.clone(), on_changed.clone()));
        }
    }

    group.upcast()
}

fn calendar_row(calendar: Calendar, store: Rc<Store>, on_changed: Rc<dyn Fn()>) -> adw::ActionRow {
    let subtitle = match (
        calendar.google_calendar_id.as_deref(),
        calendar.icloud_calendar_id.as_deref(),
    ) {
        (Some(_), _) => "Google calendar",
        (_, Some(_)) => "iCloud calendar",
        _ => "Local calendar",
    };

    let row = adw::ActionRow::builder()
        .title(gtk::glib::markup_escape_text(calendar.name.as_str()))
        .subtitle(subtitle)
        .build();
    row.set_title_lines(1);
    row.set_subtitle_lines(1);

    let swatch = gtk::DrawingArea::new();
    swatch.set_tooltip_text(Some(calendar.color.as_str()));
    swatch.set_content_width(14);
    swatch.set_content_height(14);
    swatch.set_margin_end(6);
    let color = gtk::gdk::RGBA::parse(calendar.color.as_str())
        .unwrap_or_else(|_| gtk::gdk::RGBA::new(0.2, 0.52, 0.89, 1.0));
    swatch.set_draw_func(move |_, cr, width, height| {
        cr.set_source_rgba(
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
            color.alpha() as f64,
        );
        cr.rectangle(0.0, 0.0, width as f64, height as f64);
        let _ = cr.fill();
    });
    row.add_prefix(&swatch);

    let visible_switch = gtk::Switch::builder()
        .active(calendar.visible)
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&visible_switch);
    row.set_activatable_widget(Some(&visible_switch));

    let calendar_id = calendar.id;
    visible_switch.connect_state_set(clone!(
        #[strong]
        store,
        #[strong]
        on_changed,
        move |_, state| {
            if store.set_calendar_visible(calendar_id, state).is_ok() {
                on_changed();
            }
            glib::Propagation::Proceed
        }
    ));

    row
}
