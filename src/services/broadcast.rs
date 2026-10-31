//! Broadcast recipient filtering helpers shared by bot commands and web UI.

use std::collections::HashSet;

/// Filter broadcast recipients by a segment descriptor, using existing data.
/// Segment forms: all (default), product:<id>, tag:<tag>, active:<days>,
/// minspend:<amount>.
pub fn filter_broadcast_users(
    db: &crate::db::Db,
    seller_id: Option<i64>,
    segment: &str,
) -> Vec<i64> {
    let base_users: Vec<i64> = if let Some(sid) = seller_id {
        db.list_users_for_seller(sid)
    } else {
        db.list_users()
    };
    let seg = segment.trim();
    if seg.is_empty() || seg.eq_ignore_ascii_case("all") {
        return base_users;
    }
    if let Some(pid_str) = seg.strip_prefix("product:")
        && let Ok(pid) = pid_str.trim().parse::<i64>()
    {
        let mut set: HashSet<i64> = HashSet::new();
        let invoices = db.list_invoices(seller_id, 100000);
        for (inv_id, _amount, _asset, status, _) in invoices {
            if status != "paid" && status != "executed" {
                continue;
            }
            if db.invoice_product(&inv_id) != Some(pid) {
                continue;
            }
            if let Some(info) = db.get_invoice(&inv_id) {
                let uid = info.5.unwrap_or(0);
                if uid != 0 {
                    set.insert(uid);
                }
            }
        }
        let mut out = base_users;
        out.retain(|u| set.contains(u));
        return out;
    }
    if let Some(tag) = seg.strip_prefix("tag:") {
        let tag = tag.trim();
        let mut set: HashSet<i64> = HashSet::new();
        let sellers: Vec<i64> = if let Some(sid) = seller_id {
            vec![sid]
        } else {
            db.list_sellers()
        };
        for sid in sellers {
            for c in db.list_customers(sid, Some(tag), None) {
                set.insert(c.0);
            }
        }
        let mut out = base_users;
        out.retain(|u| set.contains(u));
        return out;
    }
    if let Some(days_str) = seg.strip_prefix("active:")
        && let Ok(days) = days_str.trim().parse::<i64>()
    {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(days.max(1));
        let mut set: HashSet<i64> = HashSet::new();
        let sellers: Vec<i64> = if let Some(sid) = seller_id {
            vec![sid]
        } else {
            db.list_sellers()
        };
        for sid in sellers {
            for c in db.list_customers(sid, None, None) {
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&c.1)
                    && dt.with_timezone(&chrono::Utc) >= cutoff
                {
                    set.insert(c.0);
                }
            }
        }
        let mut out = base_users;
        out.retain(|u| set.contains(u));
        return out;
    }
    if let Some(amount_str) = seg.strip_prefix("minspend:")
        && let Ok(min) = amount_str.trim().parse::<f64>()
    {
        let mut set: HashSet<i64> = HashSet::new();
        let sellers: Vec<i64> = if let Some(sid) = seller_id {
            vec![sid]
        } else {
            db.list_sellers()
        };
        for sid in sellers {
            for c in db.list_customers(sid, None, None) {
                if c.3 >= min {
                    set.insert(c.0);
                }
            }
        }
        let mut out = base_users;
        out.retain(|u| set.contains(u));
        return out;
    }
    Vec::new()
}
