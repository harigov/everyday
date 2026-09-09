//! The two-method database a backend has to supply.
//!
//! Everything in this crate above [`Sql`] is written once and runs against
//! every driver. Everything below it is a driver: a hundred lines that know
//! how to send a statement somewhere and turn the reply into [`Row`]s.
//!
//! # Why not just use the driver's own types?
//!
//! Because there are two drivers now, and the domain code is 1,500 lines of
//! queries that should not be written twice. `rusqlite` and `postgres` do not
//! share a value type, a row type, a parameter type or a transaction type —
//! but they agree completely on what a *statement* is, and that is the only
//! thing the queries in this crate care about. So the seam is drawn at the
//! narrowest place: send SQL and arguments, get rows back.
//!
//! The cost is that rows are materialised rather than streamed. That is what
//! the previous, SQLite-only code did anyway — every query in it collected
//! into a `Vec` before decrypting, because holding the connection lock across
//! an AEAD open would stall every other query — so nothing is lost.
//!
//! # Values are deliberately loose on the way out
//!
//! [`Row::bool`] accepts an integer and [`Row::i64`] accepts a real, because
//! the two databases genuinely disagree: SQLite has no boolean type and
//! stores `starred` as `0`/`1`, while Postgres has one and gives back a
//! `bool`. Rather than make every call site ask which database it is talking
//! to, the accessors take either. Being strict here would buy nothing: a
//! column that held the wrong *kind* of thing would be caught by the JSON
//! deserialisation of the sealed payload one line later.

use everyday_core::error::{Error, Result};

/// A value on its way into a statement, or out of one.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Text(String),
    Bytes(Vec<u8>),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

/// Anything a query can be given as an argument.
///
/// A trait rather than a pile of `From` impls so that [`vals!`] can take a
/// mixture of owned and borrowed things — `id.to_string()`, `&str`, `Option`,
/// a sealed `Vec<u8>` — without the call sites converting by hand.
pub trait ToValue {
    fn to_value(&self) -> Value;
}

macro_rules! to_value {
    ($t:ty, $v:expr) => {
        impl ToValue for $t {
            fn to_value(&self) -> Value {
                #[allow(clippy::redundant_closure_call)]
                ($v)(self)
            }
        }
    };
}

to_value!(bool, |v: &bool| Value::Bool(*v));
to_value!(i64, |v: &i64| Value::Int(*v));
to_value!(i32, |v: &i32| Value::Int(i64::from(*v)));
to_value!(i16, |v: &i16| Value::Int(i64::from(*v)));
to_value!(u8, |v: &u8| Value::Int(i64::from(*v)));
to_value!(u32, |v: &u32| Value::Int(i64::from(*v)));
to_value!(f64, |v: &f64| Value::Real(*v));
to_value!(String, |v: &String| Value::Text(v.clone()));
to_value!(&str, |v: &&str| Value::Text((*v).to_string()));
to_value!(Vec<u8>, |v: &Vec<u8>| Value::Bytes(v.clone()));
to_value!(&[u8], |v: &&[u8]| Value::Bytes(v.to_vec()));
to_value!(Value, |v: &Value| v.clone());

impl<T: ToValue> ToValue for Option<T> {
    fn to_value(&self) -> Value {
        match self {
            Some(v) => v.to_value(),
            None => Value::Null,
        }
    }
}

impl<T: ToValue + ?Sized> ToValue for &T {
    fn to_value(&self) -> Value {
        (**self).to_value()
    }
}

/// Arguments for a statement, in placeholder order.
///
/// The shape `params!` had in the SQLite-only code, so the queries this
/// replaced read the same as they did.
#[macro_export]
macro_rules! vals {
    () => { ::std::vec::Vec::<$crate::conn::Value>::new() };
    ($($v:expr),+ $(,)?) => {
        ::std::vec![$($crate::conn::ToValue::to_value(&$v)),+]
    };
}

/// One row of a result set, in `SELECT` order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Row(pub Vec<Value>);

impl Row {
    fn at(&self, i: usize) -> Result<&Value> {
        self.0.get(i).ok_or_else(|| {
            Error::Invalid(format!("query returned {} columns, wanted column {i}", self.0.len()))
        })
    }

    fn wrong(&self, i: usize, want: &str) -> Error {
        Error::Invalid(format!("column {i} is not {want}: {:?}", self.0.get(i)))
    }

    pub fn text(&self, i: usize) -> Result<String> {
        match self.at(i)? {
            Value::Text(s) => Ok(s.clone()),
            _ => Err(self.wrong(i, "text")),
        }
    }

    pub fn opt_text(&self, i: usize) -> Result<Option<String>> {
        match self.at(i)? {
            Value::Null => Ok(None),
            Value::Text(s) => Ok(Some(s.clone())),
            _ => Err(self.wrong(i, "text or null")),
        }
    }

    pub fn bytes(&self, i: usize) -> Result<Vec<u8>> {
        match self.at(i)? {
            Value::Bytes(b) => Ok(b.clone()),
            _ => Err(self.wrong(i, "bytes")),
        }
    }

    /// Accepts a real too: `SUM` over an integer column is exact in SQLite
    /// and a `numeric` in Postgres, and both are integers in fact.
    pub fn i64(&self, i: usize) -> Result<i64> {
        match self.at(i)? {
            Value::Int(n) => Ok(*n),
            Value::Real(f) => Ok(*f as i64),
            _ => Err(self.wrong(i, "an integer")),
        }
    }

    pub fn opt_i64(&self, i: usize) -> Result<Option<i64>> {
        match self.at(i)? {
            Value::Null => Ok(None),
            _ => self.i64(i).map(Some),
        }
    }

    pub fn u64(&self, i: usize) -> Result<u64> {
        Ok(self.i64(i)?.max(0) as u64)
    }

    pub fn f64(&self, i: usize) -> Result<f64> {
        match self.at(i)? {
            Value::Real(f) => Ok(*f),
            Value::Int(n) => Ok(*n as f64),
            _ => Err(self.wrong(i, "a number")),
        }
    }

    pub fn opt_f64(&self, i: usize) -> Result<Option<f64>> {
        match self.at(i)? {
            Value::Null => Ok(None),
            _ => self.f64(i).map(Some),
        }
    }

    /// Accepts an integer too, which is how SQLite spells a boolean.
    pub fn bool(&self, i: usize) -> Result<bool> {
        match self.at(i)? {
            Value::Bool(b) => Ok(*b),
            Value::Int(n) => Ok(*n != 0),
            _ => Err(self.wrong(i, "a boolean")),
        }
    }
}

/// Somewhere a statement can be run: a connection, or a transaction on one.
///
/// Placeholders are always written `?1`, `?2`, … whatever the database
/// underneath spells them; the driver rewrites them on the way out. See
/// [`Dialect::bind`](crate::dialect::Dialect::bind).
pub trait Sql {
    /// Run a statement, returning how many rows it changed.
    fn execute(&mut self, sql: &str, args: &[Value]) -> Result<u64>;

    /// Run a query and materialise its rows.
    fn query(&mut self, sql: &str, args: &[Value]) -> Result<Vec<Row>>;
}

/// The shapes this crate's queries actually come in.
///
/// Nearly every read here is one of three things: a scalar count, one sealed
/// payload by id, or a list of `(id, data)` pairs to be decrypted. Naming
/// them keeps the domain modules about *what* they ask for rather than about
/// unwrapping rows.
pub trait SqlExt: Sql {
    /// The first row, or `None`. Extra rows are discarded, so this is for
    /// queries with a `WHERE id = ?1` or a `LIMIT 1` in them.
    fn query_opt(&mut self, sql: &str, args: &[Value]) -> Result<Option<Row>> {
        Ok(self.query(sql, args)?.into_iter().next())
    }

    /// A single integer: a `COUNT`, a `SUM`, a `MAX`.
    fn scalar_i64(&mut self, sql: &str, args: &[Value]) -> Result<i64> {
        match self.query_opt(sql, args)? {
            Some(row) => row.i64(0),
            // An aggregate always returns a row; anything that does not is a
            // query this helper should not have been used for.
            None => Err(Error::Invalid(format!("expected one row from: {sql}"))),
        }
    }

    /// One sealed payload, or `None` if the row is not there.
    fn sealed(&mut self, sql: &str, args: &[Value]) -> Result<Option<Vec<u8>>> {
        self.query_opt(sql, args)?.map(|r| r.bytes(0)).transpose()
    }

    /// `SELECT id, data` as the pairs [`SqlStore::collect`] wants.
    ///
    /// [`SqlStore::collect`]: crate::SqlStore::collect
    fn records(&mut self, sql: &str, args: &[Value]) -> Result<Vec<(String, Vec<u8>)>> {
        self.query(sql, args)?.into_iter().map(|r| Ok((r.text(0)?, r.bytes(1)?))).collect()
    }
}

impl<T: Sql + ?Sized> SqlExt for T {}

/// A connection this store owns for the life of the vault session.
pub trait Connection: Sql + Send {
    /// Begin a transaction. Dropping it without [`Transaction::commit`]
    /// rolls it back, which is what makes an early `?` safe.
    fn begin(&mut self) -> Result<Box<dyn Transaction + '_>>;
}

pub trait Transaction: Sql {
    fn commit(self: Box<Self>) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_convert_from_the_types_the_queries_hold() {
        let id = "b7f3".to_string();
        let sealed = vec![1u8, 2, 3];
        let args = vals![id, 7i64, 1.5f64, true, sealed, None::<String>, Some(3i32)];
        assert_eq!(
            args,
            vec![
                Value::Text("b7f3".into()),
                Value::Int(7),
                Value::Real(1.5),
                Value::Bool(true),
                Value::Bytes(vec![1, 2, 3]),
                Value::Null,
                Value::Int(3),
            ]
        );
        assert!(vals![].is_empty());
    }

    #[test]
    fn a_boolean_reads_back_from_either_spelling() {
        // Postgres answers with a boolean and SQLite with an integer, and
        // the domain code must not have to know which it is talking to.
        assert!(Row(vec![Value::Bool(true)]).bool(0).unwrap());
        assert!(Row(vec![Value::Int(1)]).bool(0).unwrap());
        assert!(!Row(vec![Value::Int(0)]).bool(0).unwrap());
    }

    #[test]
    fn a_missing_column_is_an_error_rather_than_a_panic() {
        let row = Row(vec![Value::Int(1)]);
        assert!(row.text(4).is_err());
        assert!(row.i64(0).is_ok());
    }

    #[test]
    fn null_and_present_are_distinguished_on_the_way_out() {
        let row = Row(vec![Value::Null, Value::Int(9), Value::Real(2.5)]);
        assert_eq!(row.opt_i64(0).unwrap(), None);
        assert_eq!(row.opt_i64(1).unwrap(), Some(9));
        assert_eq!(row.opt_f64(2).unwrap(), Some(2.5));
        assert_eq!(row.opt_text(0).unwrap(), None);
        // A number is not text, and saying so beats returning "9".
        assert!(row.text(1).is_err());
    }
}
