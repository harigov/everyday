//! Two small shapes that recurred wherever a part exports one file per
//! record beside an index CSV describing all of them: turning that index
//! into a lookup by file name, and finding a name a previous record has not
//! already taken.

use crate::Result;
use crate::text::{Row, Table};
use std::collections::BTreeMap;

/// Read an index CSV's rows into a map from the file name each one names to
/// whatever `parse` makes of it.
///
/// A row `parse` cannot use -- an id that will not parse, most often -- is
/// left out rather than failing the whole index: the file it named still
/// falls to a document loop's own fallback, which is what keeps a
/// hand-edited index recoverable rather than fatal.
///
/// `entry().or_insert()` rather than a plain insert, so that if two rows
/// somehow name the same file the first one written keeps it -- matching a
/// list searched in order, which is the map this replaced in one of the
/// three parts that had one.
pub fn index_map<T>(
    text: Option<&str>,
    mut parse: impl FnMut(&Row<'_>) -> Option<(String, T)>,
) -> BTreeMap<String, T> {
    let mut map = BTreeMap::new();
    if let Some(text) = text {
        for row in Table::parse(text).rows() {
            if let Some((file, value)) = parse(&row) {
                map.entry(file).or_insert(value);
            }
        }
    }
    map
}

/// The record a document belongs to: the one an index mapped its file name
/// to, or -- for a file the index never named, which is what a folder from
/// another program looks like -- whatever `named` finds or mints from the
/// file's own name. Remembered afterwards, in case the same file name comes
/// up again.
pub fn owner<Id: Copy>(
    index: &mut BTreeMap<String, Id>,
    file: &str,
    named: impl FnOnce() -> Result<Id>,
) -> Result<Id> {
    if let Some(id) = index.get(file) {
        return Ok(*id);
    }
    let id = named()?;
    index.insert(file.to_string(), id);
    Ok(id)
}

/// Makes a name unique against every one it has already handed out, by
/// whatever rule the caller wants for what a second try looks like.
///
/// Four parts solved "two records reduce to the same file name" -- a journal
/// losing its capitals, two shelves sharing a title -- and each solved it
/// with its own suffix: `-2`, `-3` for a name a person reads; an id's own
/// short form for one that is mostly a hint. `Namer` is the "try it, and if
/// it is taken try again" loop all four wrote by hand; which suffix a
/// collision gets stays the part's own choice, passed in as `retry`.
pub struct Namer {
    taken: Vec<String>,
}

impl Namer {
    pub fn new() -> Self {
        Self { taken: Vec::new() }
    }

    /// `base` if nobody has it yet, otherwise `retry(base, n)` for
    /// `n = 2, 3, ...` until one is free.
    pub fn unique(&mut self, base: &str, mut retry: impl FnMut(&str, u32) -> String) -> String {
        let mut name = base.to_string();
        let mut n = 2;
        while self.taken.contains(&name) {
            name = retry(base, n);
            n += 1;
        }
        self.taken.push(name.clone());
        name
    }
}

impl Default for Namer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_namer_only_adds_a_suffix_once_it_has_to() {
        let mut namer = Namer::new();
        assert_eq!(namer.unique("Books", |base, n| format!("{base}-{n}")), "Books");
        assert_eq!(namer.unique("Books", |base, n| format!("{base}-{n}")), "Books-2");
        assert_eq!(namer.unique("Books", |base, n| format!("{base}-{n}")), "Books-3");
    }

    #[test]
    fn owner_mints_once_and_remembers_the_id_it_minted() {
        let mut index = BTreeMap::new();
        let mut minted = 0;
        let mut mint = || -> Result<u32> {
            minted += 1;
            Ok(minted)
        };
        assert_eq!(owner(&mut index, "Inbox.md", &mut mint).unwrap(), 1);
        assert_eq!(owner(&mut index, "Inbox.md", &mut mint).unwrap(), 1);
        assert_eq!(minted, 1, "the second lookup should not have minted again");
    }

    #[test]
    fn index_map_keeps_the_first_row_that_names_a_file() {
        let csv = "file,id\nBooks.csv,1\nBooks.csv,2\n";
        let map = index_map(Some(csv), |row| {
            Some((row.get("file").to_string(), row.get("id").to_string()))
        });
        assert_eq!(map.get("Books.csv").map(String::as_str), Some("1"));
    }
}
