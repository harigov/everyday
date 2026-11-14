//! Rules: "anything from this payee is that", applied as transactions
//! arrive. Pure functions over records; the vault decides when to run them.
//!
//! # The order things are decided in
//!
//! 1. A person's choice ([`CategorizedBy::Person`]) is final. Nothing here
//!    touches a transaction whose category is the person's -- not its
//!    category, and not its payee or exclusion either.
//! 2. Otherwise the first matching rule, by `sort_order` then creation, sets
//!    whatever it sets: its category (and [`CategorizedBy::Rule`]), its
//!    rename into `payee_override` when that is empty, its exclusion.
//! 3. Otherwise the bank's own category, through
//!    [`super::defaults::for_bank_category`] -- which is the vault's job,
//!    since it needs the vault's categories by name.
//!
//! # What a rule matches against
//!
//! The bank's payee and description, never the person's own name for the
//! payee. A rule is written against what the bank sends ("AMZN MKTP"), and a
//! rename -- a rule's or a person's -- must not change which rules match the
//! next time they run, or the order rules are listed in would stop meaning
//! anything.

use super::{CategorizedBy, MoneyRule, Txn};
use crate::error::{Error, Result};
use jiff::Timestamp;

/// The longest a rule's text or rename may be, in characters. A payee is a
/// few words; anything longer is a paste gone wrong.
pub const MAX_RULE_TEXT: usize = 200;

/// Does `rule` match `txn`? Case-insensitive; `exact` compares the whole
/// bank payee (trimmed), otherwise a substring of payee or description.
///
/// A rule with blank text matches nothing -- [`validate`] refuses one, but a
/// stored record is outside data by the time it is read back, and a blank
/// needle is a substring of everything.
pub fn matches(rule: &MoneyRule, txn: &Txn) -> bool {
    let needle = rule.text.trim().to_lowercase();
    if needle.is_empty() {
        return false;
    }
    if rule.account_id.is_some_and(|account| account != txn.account_id) {
        return false;
    }
    if rule.min_amount.is_some_and(|min| txn.amount < min) {
        return false;
    }
    if rule.max_amount.is_some_and(|max| txn.amount > max) {
        return false;
    }
    if rule.exact {
        txn.payee.trim().to_lowercase() == needle
    } else {
        txn.payee.to_lowercase().contains(&needle)
            || txn.description.to_lowercase().contains(&needle)
    }
}

/// Apply the first rule in `rules` (already in order) that matches, if the
/// transaction's category is still open. Returns whether anything changed.
///
/// Only the first match acts, even when it sets less than a later one would
/// (a rename-only rule ahead of a categorising one): the list's order is the
/// person's statement of which rule they meant, and two rules each doing
/// half of what they say would be impossible to read back from the list.
///
/// For a transaction *arriving* -- from a sync, a file, a person typing it
/// -- or one rule applied on purpose over what is already there. A rule's
/// rename and exclusion land here and nowhere else: once a transaction is
/// stored, its payee name and whether it counts are the person's, and a
/// person who counted a transaction again, or cleared a name, must not see
/// a later "Run rules" undo it. See [`recategorize`] for that run.
pub fn apply(rules: &[MoneyRule], txn: &mut Txn) -> bool {
    if !txn.category_is_open() {
        return false;
    }
    let Some(rule) = rules.iter().find(|r| matches(r, txn)) else {
        return false;
    };
    let mut changed = set_category(rule, txn);
    let rename = rule.rename.trim();
    if !rename.is_empty() && txn.payee_override.trim().is_empty() {
        txn.payee_override = rename.to_string();
        changed = true;
    }
    if rule.exclude && !txn.excluded {
        txn.excluded = true;
        changed = true;
    }
    if changed {
        txn.updated_at = Timestamp::now();
    }
    changed
}

/// The first matching rule's *category*, on a transaction already stored:
/// what "Run rules" does over the history. Leaves its payee name and its
/// exclusion as they are, which by then are the person's -- see [`apply`].
pub fn recategorize(rules: &[MoneyRule], txn: &mut Txn) -> bool {
    if !txn.category_is_open() {
        return false;
    }
    let Some(rule) = rules.iter().find(|r| matches(r, txn)) else {
        return false;
    };
    let changed = set_category(rule, txn);
    if changed {
        txn.updated_at = Timestamp::now();
    }
    changed
}

/// `rule`'s category on `txn`, if it sets one. Whether that changed it.
fn set_category(rule: &MoneyRule, txn: &mut Txn) -> bool {
    let Some(category) = rule.category else { return false };
    let by = CategorizedBy::Rule { rule_id: rule.id };
    if txn.category == Some(category) && txn.categorized_by == by {
        return false;
    }
    txn.category = Some(category);
    txn.categorized_by = by;
    true
}

/// Sort rules into the order [`apply`] expects: `sort_order`, then
/// `created_at`, then id.
pub fn in_order(rules: &mut [MoneyRule]) {
    rules.sort_by(|a, b| {
        a.sort_order.cmp(&b.sort_order).then(a.created_at.cmp(&b.created_at)).then(a.id.cmp(&b.id))
    });
}

/// Refuse a rule that would match everything or nothing: blank text, a
/// range whose minimum is above its maximum, or a rule that does nothing.
/// Also anything longer than [`MAX_RULE_TEXT`], since both strings are
/// typed by a person (or the assistant on their behalf).
pub fn validate(rule: &MoneyRule) -> Result<()> {
    let text = rule.text.trim();
    if text.is_empty() {
        return Err(Error::Invalid("a rule needs some text to look for".into()));
    }
    if text.chars().count() > MAX_RULE_TEXT {
        return Err(Error::Invalid(format!(
            "a rule's text can be at most {MAX_RULE_TEXT} characters"
        )));
    }
    if rule.rename.trim().chars().count() > MAX_RULE_TEXT {
        return Err(Error::Invalid(format!(
            "a rule's new payee name can be at most {MAX_RULE_TEXT} characters"
        )));
    }
    if let (Some(min), Some(max)) = (rule.min_amount, rule.max_amount)
        && min > max
    {
        return Err(Error::Invalid(
            "a rule's smallest amount is larger than its largest, so it would match nothing".into(),
        ));
    }
    if rule.category.is_none() && rule.rename.trim().is_empty() && !rule.exclude {
        return Err(Error::Invalid(
            "a rule needs to do something: set a category, rename the payee, or exclude".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{BankAccountId, MoneyCategoryId};
    use jiff::civil::Date;

    fn txn(payee: &str, amount: i64) -> Txn {
        let mut t = Txn::new(BankAccountId::new(), "k", Date::constant(2026, 11, 1), amount, "USD");
        t.payee = payee.into();
        t
    }

    fn rule(text: &str, category: MoneyCategoryId) -> MoneyRule {
        let mut r = MoneyRule::new(text);
        r.category = Some(category);
        r
    }

    #[test]
    fn a_rule_matches_payee_or_description_ignoring_case() {
        let r = rule("netflix", MoneyCategoryId::new());
        assert!(matches(&r, &txn("NETFLIX.COM", -1599)));
        let mut by_description = txn("CARD PURCHASE", -1599);
        by_description.description = "POS Netflix 4421".into();
        assert!(matches(&r, &by_description));
        assert!(!matches(&r, &txn("Hulu", -1599)));
    }

    #[test]
    fn an_exact_rule_wants_the_whole_payee() {
        let mut r = rule("Shell", MoneyCategoryId::new());
        r.exact = true;
        assert!(matches(&r, &txn("  shell ", -4000)));
        assert!(!matches(&r, &txn("Shell Oil 1234", -4000)));
        // Not the description either.
        let mut t = txn("Fuel", -4000);
        t.description = "Shell".into();
        assert!(!matches(&r, &t));
    }

    #[test]
    fn a_rule_matches_the_banks_payee_not_the_persons_rename() {
        let r = rule("amzn", MoneyCategoryId::new());
        let mut t = txn("AMZN MKTP US", -2500);
        t.payee_override = "Amazon".into();
        assert!(matches(&r, &t));
        let mut renamed_only = txn("Bookshop", -2500);
        renamed_only.payee_override = "amzn".into();
        assert!(!matches(&r, &renamed_only));
    }

    #[test]
    fn account_and_amount_limits_narrow_a_rule() {
        let mut r = rule("transfer", MoneyCategoryId::new());
        let t = txn("Transfer to savings", -50_000);
        r.account_id = Some(BankAccountId::new());
        assert!(!matches(&r, &t), "another account");
        r.account_id = Some(t.account_id);
        assert!(matches(&r, &t));
        r.min_amount = Some(-10_000);
        assert!(!matches(&r, &t), "below the minimum");
        r.min_amount = Some(-60_000);
        r.max_amount = Some(-50_000);
        assert!(matches(&r, &t), "the range is inclusive");
        r.max_amount = Some(-50_001);
        assert!(!matches(&r, &t), "above the maximum");
    }

    #[test]
    fn a_blank_rule_matches_nothing_even_if_it_was_stored() {
        let r = rule("   ", MoneyCategoryId::new());
        assert!(!matches(&r, &txn("anything", -1)));
    }

    #[test]
    fn the_first_matching_rule_wins() {
        let (coffee, food) = (MoneyCategoryId::new(), MoneyCategoryId::new());
        let mut first = rule("starbucks", coffee);
        first.sort_order = 0;
        let mut second = rule("star", food);
        second.sort_order = 1;
        let mut rules = vec![second, first.clone()];
        in_order(&mut rules);
        let mut t = txn("STARBUCKS #123", -450);
        assert!(apply(&rules, &mut t));
        assert_eq!(t.category, Some(coffee));
        assert_eq!(t.categorized_by, CategorizedBy::Rule { rule_id: first.id });
    }

    #[test]
    fn a_persons_choice_is_never_touched_by_a_rule() {
        let mine = MoneyCategoryId::new();
        let mut r = rule("starbucks", MoneyCategoryId::new());
        r.rename = "Starbucks".into();
        r.exclude = true;
        let mut t = txn("STARBUCKS #123", -450);
        t.category = Some(mine);
        t.categorized_by = CategorizedBy::Person;
        let before = t.clone();
        assert!(!apply(&[r], &mut t));
        assert_eq!(t, before, "not its category, payee or exclusion");
    }

    #[test]
    fn a_rule_improves_on_the_banks_category() {
        let (bank, rules_choice) = (MoneyCategoryId::new(), MoneyCategoryId::new());
        let r = rule("costco", rules_choice);
        let mut t = txn("COSTCO WHOLESALE", -12_000);
        t.category = Some(bank);
        t.categorized_by = CategorizedBy::Bank;
        assert!(apply(std::slice::from_ref(&r), &mut t));
        assert_eq!(t.category, Some(rules_choice));
        assert_eq!(t.categorized_by, CategorizedBy::Rule { rule_id: r.id });
    }

    #[test]
    fn a_rename_fills_only_an_empty_payee_override() {
        let mut r = MoneyRule::new("amzn");
        r.rename = "  Amazon ".into();
        let mut t = txn("AMZN MKTP", -999);
        assert!(apply(std::slice::from_ref(&r), &mut t));
        assert_eq!(t.payee_override, "Amazon");
        assert_eq!(t.category, None, "a rename-only rule sets no category");

        let mut named = txn("AMZN MKTP", -999);
        named.payee_override = "Books".into();
        assert!(!apply(&[r], &mut named));
        assert_eq!(named.payee_override, "Books");
    }

    #[test]
    fn an_excluding_rule_excludes() {
        let mut r = MoneyRule::new("venmo");
        r.exclude = true;
        let mut t = txn("Venmo", -2000);
        assert!(apply(std::slice::from_ref(&r), &mut t));
        assert!(t.excluded);
        assert!(!apply(&[r], &mut t), "applying again changes nothing");
    }

    #[test]
    fn nothing_matching_changes_nothing() {
        let r = rule("netflix", MoneyCategoryId::new());
        let mut t = txn("Hulu", -1599);
        let before = t.clone();
        assert!(!apply(&[r], &mut t));
        assert_eq!(t, before);
    }

    #[test]
    fn rules_sort_by_order_then_age() {
        let mut a = MoneyRule::new("a");
        let mut b = MoneyRule::new("b");
        let mut c = MoneyRule::new("c");
        a.sort_order = 2;
        b.sort_order = 1;
        c.sort_order = 1;
        b.created_at = Timestamp::from_second(1_000).unwrap();
        c.created_at = Timestamp::from_second(500).unwrap();
        let mut rules = vec![a, b, c];
        in_order(&mut rules);
        let order: Vec<_> = rules.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(order, ["c", "b", "a"]);
    }

    #[test]
    fn a_rule_that_would_match_everything_or_nothing_or_do_nothing_is_refused() {
        let category = MoneyCategoryId::new();
        assert!(validate(&rule("  ", category)).is_err(), "blank text");
        let mut backwards = rule("x", category);
        backwards.min_amount = Some(100);
        backwards.max_amount = Some(-100);
        assert!(validate(&backwards).is_err(), "min above max");
        assert!(validate(&MoneyRule::new("x")).is_err(), "does nothing");
        assert!(validate(&rule(&"x".repeat(MAX_RULE_TEXT + 1), category)).is_err(), "too long");
        let mut long_rename = MoneyRule::new("x");
        long_rename.rename = "y".repeat(MAX_RULE_TEXT + 1);
        assert!(validate(&long_rename).is_err(), "rename too long");

        assert!(validate(&rule("netflix", category)).is_ok());
        let mut excluding = MoneyRule::new("venmo");
        excluding.exclude = true;
        assert!(validate(&excluding).is_ok());
        let mut renaming = MoneyRule::new("amzn");
        renaming.rename = "Amazon".into();
        assert!(validate(&renaming).is_ok());
    }

    #[test]
    fn a_rerun_sets_categories_and_leaves_a_persons_exclusion_and_name_alone() {
        let account = crate::id::BankAccountId::new();
        let mut rule = MoneyRule::new("venmo");
        rule.category = Some(crate::id::MoneyCategoryId::new());
        rule.exclude = true;
        rule.rename = "Venmo".into();
        let mut t = Txn::new(account, "k", jiff::civil::date(2026, 11, 1), -500, "USD");
        t.payee = "VENMO PAYMENT".into();
        assert!(apply(std::slice::from_ref(&rule), &mut t), "arriving: everything lands");
        assert!(t.excluded && t.payee_override == "Venmo");

        // The person counts it again and clears the name; a re-run keeps both.
        t.excluded = false;
        t.payee_override.clear();
        assert!(!recategorize(std::slice::from_ref(&rule), &mut t));
        assert!(!t.excluded && t.payee_override.is_empty());
    }
}
