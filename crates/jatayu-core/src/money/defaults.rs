//! What a new vault's Money starts with: a starter set of categories, and
//! how the bank's own categories map onto them.
//!
//! Plaid labels every transaction with a "personal finance category" --
//! `FOOD_AND_DRINK`, `FOOD_AND_DRINK_RESTAURANT` -- and most of the time that
//! label is right. Mapping it onto a starter category means a freshly linked
//! bank arrives mostly sorted, and a person spends their first five minutes
//! correcting the exceptions rather than labelling three hundred lines. The
//! mapping only ever names a *starter* category, by name; a vault whose
//! person renamed or deleted it simply gets no category from the bank for
//! those, and the rules and the person do the rest.
//!
//! # Where the mapping deliberately says nothing
//!
//! A few of Plaid's labels are honest about not knowing what something was,
//! and guessing would hide it. A cash or cheque deposit
//! (`TRANSFER_IN_DEPOSIT`) might be a paycheque, a friend paying back
//! dinner, or one's own cash going back in; calling it a transfer would take
//! it out of income, and calling it income might count the same money
//! twice. An ATM withdrawal (`TRANSFER_OUT_WITHDRAWAL`) is spending that has
//! not happened yet. Both arrive uncategorised, which the reports count by
//! direction -- money out as spending, money in as income -- and which the
//! Overview lists as needing a look. The same goes for the catch-all
//! primaries (`GENERAL_SERVICES`, `GOVERNMENT_AND_NON_PROFIT`,
//! `TRANSPORTATION`) whose detail did not say more: "Other" is a category a
//! person never revisits, and an empty one is a question they will answer.

use super::{CategoryKind, MoneyCategory};

/// `(group, name, kind)` for every starter category, in the order a new
/// vault lists them. One table, so the names [`for_bank_category`] returns
/// can be checked against it in a test rather than by eye.
const STARTERS: &[(&str, &str, CategoryKind)] = &[
    ("Income", "Paycheck", CategoryKind::Income),
    ("Income", "Interest", CategoryKind::Income),
    ("Income", "Other income", CategoryKind::Income),
    ("Transfers", "Transfer", CategoryKind::Transfer),
    ("Transfers", "Credit card payment", CategoryKind::Transfer),
    ("Home", "Rent & mortgage", CategoryKind::Expense),
    ("Home", "Utilities", CategoryKind::Expense),
    ("Home", "Home improvement", CategoryKind::Expense),
    ("Home", "Insurance", CategoryKind::Expense),
    ("Food", "Groceries", CategoryKind::Expense),
    ("Food", "Restaurants", CategoryKind::Expense),
    ("Food", "Coffee", CategoryKind::Expense),
    ("Transport", "Fuel", CategoryKind::Expense),
    ("Transport", "Public transit", CategoryKind::Expense),
    ("Transport", "Rideshare & taxi", CategoryKind::Expense),
    ("Transport", "Parking", CategoryKind::Expense),
    ("Transport", "Car", CategoryKind::Expense),
    ("Shopping", "General merchandise", CategoryKind::Expense),
    ("Shopping", "Clothing", CategoryKind::Expense),
    ("Shopping", "Electronics", CategoryKind::Expense),
    ("Health", "Medical", CategoryKind::Expense),
    ("Health", "Pharmacy", CategoryKind::Expense),
    ("Health", "Fitness", CategoryKind::Expense),
    ("Personal", "Personal care", CategoryKind::Expense),
    ("Personal", "Subscriptions", CategoryKind::Expense),
    ("Personal", "Entertainment", CategoryKind::Expense),
    ("Personal", "Education", CategoryKind::Expense),
    ("Personal", "Gifts & donations", CategoryKind::Expense),
    ("Travel", "Travel", CategoryKind::Expense),
    ("Fees", "Bank fees", CategoryKind::Expense),
    ("Fees", "Taxes", CategoryKind::Expense),
    ("Fees", "Loan payments", CategoryKind::Expense),
    ("Other", "Other", CategoryKind::Expense),
];

/// The categories `Vault::ensure_money_defaults` writes into a vault that
/// has none, in the order they are listed. Names are unique
/// case-insensitively; groups are what the budget view heads them with.
///
/// Fresh ids every call: these are records about to be written, not
/// constants to compare against. Nothing has a budget -- a number a person
/// did not choose is a budget they will not trust.
pub fn starter_categories() -> Vec<MoneyCategory> {
    STARTERS
        .iter()
        .enumerate()
        .map(|(i, (group, name, kind))| {
            let mut category = MoneyCategory::new(*name, *group, *kind);
            category.sort_order = i as i64;
            category
        })
        .collect()
}

/// The starter category's name that a bank category maps to, if any.
///
/// `bank_category` is [`super::Txn::bank_category`]: Plaid's primary and
/// detailed personal finance category joined with `/`, e.g.
/// `FOOD_AND_DRINK/FOOD_AND_DRINK_RESTAURANT`, or just the primary. The
/// detailed label is consulted first, then the primary.
///
/// Read leniently, because the string comes from outside: case and
/// surrounding space do not matter, the halves may come in either order, and
/// a detailed label on its own still finds its primary by prefix.
pub fn for_bank_category(bank_category: &str) -> Option<&'static str> {
    let upper = bank_category.trim().to_ascii_uppercase();
    if upper.is_empty() {
        return None;
    }
    let parts: Vec<&str> = upper.split('/').map(str::trim).filter(|p| !p.is_empty()).collect();

    // Detailed first: whichever part is a known detailed label decides,
    // including when it decides "nothing" (see the module doc).
    for part in parts.iter().rev() {
        if let Some(answer) = detailed(part) {
            return answer;
        }
    }
    for part in parts.iter().rev() {
        if let Some(name) = primary(part) {
            return Some(name);
        }
    }
    // A detailed label this table does not list -- Plaid adds them -- still
    // starts with its primary's name.
    for part in parts.iter().rev() {
        if let Some(name) = PRIMARIES
            .iter()
            .filter(|p| {
                part.len() > p.len() && part.starts_with(**p) && part.as_bytes()[p.len()] == b'_'
            })
            .max_by_key(|p| p.len())
            .copied()
            .and_then(primary)
        {
            return Some(name);
        }
    }
    None
}

/// Plaid's primary personal finance categories. Listed so a detailed label
/// the table below has never seen can still be read by its prefix.
const PRIMARIES: &[&str] = &[
    "INCOME",
    "TRANSFER_IN",
    "TRANSFER_OUT",
    "LOAN_PAYMENTS",
    "BANK_FEES",
    "ENTERTAINMENT",
    "FOOD_AND_DRINK",
    "GENERAL_MERCHANDISE",
    "HOME_IMPROVEMENT",
    "MEDICAL",
    "PERSONAL_CARE",
    "GENERAL_SERVICES",
    "GOVERNMENT_AND_NON_PROFIT",
    "TRANSPORTATION",
    "TRAVEL",
    "RENT_AND_UTILITIES",
];

/// A primary category's starter, when the primary alone says enough.
fn primary(label: &str) -> Option<&'static str> {
    Some(match label {
        "INCOME" => "Other income",
        "TRANSFER_IN" | "TRANSFER_OUT" => "Transfer",
        "LOAN_PAYMENTS" => "Loan payments",
        "BANK_FEES" => "Bank fees",
        "ENTERTAINMENT" => "Entertainment",
        "FOOD_AND_DRINK" => "Restaurants",
        "GENERAL_MERCHANDISE" => "General merchandise",
        "HOME_IMPROVEMENT" => "Home improvement",
        "MEDICAL" => "Medical",
        "PERSONAL_CARE" => "Personal care",
        "TRAVEL" => "Travel",
        "RENT_AND_UTILITIES" => "Utilities",
        // GENERAL_SERVICES, GOVERNMENT_AND_NON_PROFIT, TRANSPORTATION: too
        // broad to guess -- see the module doc.
        _ => return None,
    })
}

/// A detailed category's starter. `None` when the label is not one this
/// table knows (the primary decides); `Some(None)` when it is known and
/// deliberately maps to nothing, overriding its primary.
fn detailed(label: &str) -> Option<Option<&'static str>> {
    Some(Some(match label {
        "INCOME_WAGES" => "Paycheck",
        "INCOME_INTEREST_EARNED" => "Interest",

        "TRANSFER_IN_DEPOSIT" | "TRANSFER_OUT_WITHDRAWAL" => return Some(None),

        "LOAN_PAYMENTS_CREDIT_CARD_PAYMENT" => "Credit card payment",
        "LOAN_PAYMENTS_MORTGAGE_PAYMENT" => "Rent & mortgage",
        "LOAN_PAYMENTS_CAR_PAYMENT" => "Car",

        "FOOD_AND_DRINK_GROCERIES" | "FOOD_AND_DRINK_BEER_WINE_AND_LIQUOR" => "Groceries",
        "FOOD_AND_DRINK_RESTAURANT" | "FOOD_AND_DRINK_FAST_FOOD" => "Restaurants",
        "FOOD_AND_DRINK_COFFEE" => "Coffee",

        "GENERAL_MERCHANDISE_CLOTHING_AND_ACCESSORIES" => "Clothing",
        "GENERAL_MERCHANDISE_ELECTRONICS" => "Electronics",
        "GENERAL_MERCHANDISE_GIFTS_AND_NOVELTIES" => "Gifts & donations",

        "MEDICAL_PHARMACIES_AND_SUPPLEMENTS" => "Pharmacy",

        "PERSONAL_CARE_GYMS_AND_FITNESS_CENTERS" => "Fitness",

        "GENERAL_SERVICES_INSURANCE" => "Insurance",
        "GENERAL_SERVICES_EDUCATION" => "Education",
        "GENERAL_SERVICES_AUTOMOTIVE" => "Car",

        "GOVERNMENT_AND_NON_PROFIT_TAX_PAYMENT" => "Taxes",
        "GOVERNMENT_AND_NON_PROFIT_DONATIONS" => "Gifts & donations",

        "TRANSPORTATION_GAS" => "Fuel",
        "TRANSPORTATION_PUBLIC_TRANSIT" => "Public transit",
        "TRANSPORTATION_TAXIS_AND_RIDE_SHARES" => "Rideshare & taxi",
        "TRANSPORTATION_PARKING" => "Parking",
        "TRANSPORTATION_TOLLS" => "Car",

        "RENT_AND_UTILITIES_RENT" => "Rent & mortgage",

        _ => return None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Every label the tables above mention, so the "names exist" test
    /// walks all of them rather than a sample.
    const EVERY_LABEL: &[&str] = &[
        "INCOME",
        "INCOME_WAGES",
        "INCOME_INTEREST_EARNED",
        "INCOME_DIVIDENDS",
        "TRANSFER_IN",
        "TRANSFER_IN_DEPOSIT",
        "TRANSFER_IN_SAVINGS",
        "TRANSFER_OUT",
        "TRANSFER_OUT_WITHDRAWAL",
        "TRANSFER_OUT_SAVINGS",
        "LOAN_PAYMENTS",
        "LOAN_PAYMENTS_CREDIT_CARD_PAYMENT",
        "LOAN_PAYMENTS_MORTGAGE_PAYMENT",
        "LOAN_PAYMENTS_CAR_PAYMENT",
        "LOAN_PAYMENTS_STUDENT_LOAN_PAYMENT",
        "BANK_FEES",
        "BANK_FEES_ATM_FEES",
        "BANK_FEES_OVERDRAFT_FEES",
        "BANK_FEES_INTEREST_CHARGE",
        "BANK_FEES_FOREIGN_TRANSACTION_FEES",
        "BANK_FEES_INSUFFICIENT_FUNDS",
        "ENTERTAINMENT",
        "ENTERTAINMENT_TV_AND_MOVIES",
        "FOOD_AND_DRINK",
        "FOOD_AND_DRINK_GROCERIES",
        "FOOD_AND_DRINK_BEER_WINE_AND_LIQUOR",
        "FOOD_AND_DRINK_RESTAURANT",
        "FOOD_AND_DRINK_FAST_FOOD",
        "FOOD_AND_DRINK_COFFEE",
        "GENERAL_MERCHANDISE",
        "GENERAL_MERCHANDISE_CLOTHING_AND_ACCESSORIES",
        "GENERAL_MERCHANDISE_ELECTRONICS",
        "GENERAL_MERCHANDISE_GIFTS_AND_NOVELTIES",
        "HOME_IMPROVEMENT",
        "HOME_IMPROVEMENT_HARDWARE",
        "MEDICAL",
        "MEDICAL_PHARMACIES_AND_SUPPLEMENTS",
        "PERSONAL_CARE",
        "PERSONAL_CARE_GYMS_AND_FITNESS_CENTERS",
        "GENERAL_SERVICES",
        "GENERAL_SERVICES_INSURANCE",
        "GENERAL_SERVICES_EDUCATION",
        "GENERAL_SERVICES_AUTOMOTIVE",
        "GOVERNMENT_AND_NON_PROFIT",
        "GOVERNMENT_AND_NON_PROFIT_TAX_PAYMENT",
        "GOVERNMENT_AND_NON_PROFIT_DONATIONS",
        "TRANSPORTATION",
        "TRANSPORTATION_GAS",
        "TRANSPORTATION_PUBLIC_TRANSIT",
        "TRANSPORTATION_TAXIS_AND_RIDE_SHARES",
        "TRANSPORTATION_PARKING",
        "TRANSPORTATION_TOLLS",
        "TRAVEL",
        "TRAVEL_FLIGHTS",
        "RENT_AND_UTILITIES",
        "RENT_AND_UTILITIES_RENT",
        "RENT_AND_UTILITIES_GAS_AND_ELECTRICITY",
    ];

    #[test]
    fn every_name_the_bank_mapping_returns_is_a_starter_category() {
        let names: HashSet<String> =
            starter_categories().into_iter().map(|c| c.name.to_lowercase()).collect();
        for label in EVERY_LABEL {
            // As Plaid's two halves arrive joined, and on its own.
            let its_primary = PRIMARIES
                .iter()
                .filter(|p| label.starts_with(**p))
                .max_by_key(|p| p.len())
                .copied()
                .unwrap_or(*label);
            for form in [label.to_string(), format!("{its_primary}/{label}")] {
                if let Some(name) = for_bank_category(&form) {
                    assert!(names.contains(&name.to_lowercase()), "{form} → {name}, not a starter");
                }
            }
        }
        for p in PRIMARIES {
            if let Some(name) = for_bank_category(p) {
                assert!(names.contains(&name.to_lowercase()), "{p} → {name}, not a starter");
            }
        }
    }

    #[test]
    fn starter_names_are_unique_case_insensitively() {
        let starters = starter_categories();
        let mut seen = HashSet::new();
        for c in &starters {
            assert!(seen.insert(c.name.to_lowercase()), "{} is listed twice", c.name);
            assert!(!c.name.trim().is_empty());
            assert_eq!(c.budget, None, "a starter never comes with a budget");
        }
    }

    #[test]
    fn starters_are_ordered_as_listed() {
        let starters = starter_categories();
        for (i, c) in starters.iter().enumerate() {
            assert_eq!(c.sort_order, i as i64);
        }
    }

    #[test]
    fn the_detailed_label_wins_over_the_primary() {
        assert_eq!(for_bank_category("FOOD_AND_DRINK/FOOD_AND_DRINK_GROCERIES"), Some("Groceries"));
        assert_eq!(for_bank_category("FOOD_AND_DRINK/FOOD_AND_DRINK_COFFEE"), Some("Coffee"));
        assert_eq!(for_bank_category("FOOD_AND_DRINK"), Some("Restaurants"));
        assert_eq!(
            for_bank_category("LOAN_PAYMENTS/LOAN_PAYMENTS_CREDIT_CARD_PAYMENT"),
            Some("Credit card payment")
        );
        assert_eq!(for_bank_category("INCOME/INCOME_WAGES"), Some("Paycheck"));
        assert_eq!(for_bank_category("BANK_FEES/BANK_FEES_ATM_FEES"), Some("Bank fees"));
    }

    #[test]
    fn a_deposit_or_withdrawal_of_cash_is_left_for_the_person() {
        // The primary would say "Transfer", which would hide a paycheque
        // paid in by cheque from income.
        assert_eq!(for_bank_category("TRANSFER_IN/TRANSFER_IN_DEPOSIT"), None);
        assert_eq!(for_bank_category("TRANSFER_OUT/TRANSFER_OUT_WITHDRAWAL"), None);
        assert_eq!(for_bank_category("TRANSFER_IN/TRANSFER_IN_SAVINGS"), Some("Transfer"));
    }

    #[test]
    fn an_unknown_detailed_label_falls_back_to_its_primary_by_prefix() {
        assert_eq!(for_bank_category("TRAVEL_SPACE_FLIGHTS"), Some("Travel"));
        assert_eq!(for_bank_category("MEDICAL_SOMETHING_NEW"), Some("Medical"));
        // Not a word boundary: no prefix match.
        assert_eq!(for_bank_category("TRAVELLING"), None);
    }

    #[test]
    fn the_mapping_reads_leniently_and_refuses_nonsense() {
        assert_eq!(for_bank_category("  food_and_drink / food_and_drink_coffee "), Some("Coffee"));
        assert_eq!(for_bank_category(""), None);
        assert_eq!(for_bank_category("/"), None);
        assert_eq!(for_bank_category("SOMETHING_ELSE"), None);
        assert_eq!(for_bank_category("GENERAL_SERVICES"), None);
    }
}
