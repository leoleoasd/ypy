use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3::PyIterProtocol;
use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::ops::DerefMut;
use yrs::types::map::{MapEvent, MapIter};
use yrs::{Map, Transaction};

use crate::shared_types::SharedType;
use crate::type_conversions::{PyValueWrapper, ToPython};
use crate::y_transaction::YTransaction;

/// Collection used to store key-value entries in an unordered manner. Keys are always represented
/// as UTF-8 strings. Values can be any value type supported by Yrs: JSON-like primitives as well as
/// shared data types.
///
/// In terms of conflict resolution, [Map] uses logical last-write-wins principle, meaning the past
/// updates are automatically overridden and discarded by newer ones, while concurrent updates made
/// by different peers are resolved into a single value using document id seniority to establish
/// order.
#[pyclass(unsendable)]
pub struct YMap(pub SharedType<Map, HashMap<String, PyObject>>);

impl From<Map> for YMap {
    fn from(v: Map) -> Self {
        YMap(SharedType::new(v))
    }
}

#[pymethods]
impl YMap {
    /// Creates a new preliminary instance of a `YMap` shared data type, with its state
    /// initialized to provided parameter.
    ///
    /// Preliminary instances can be nested into other shared data types such as `YArray` and `YMap`.
    /// Once a preliminary instance has been inserted this way, it becomes integrated into y-py
    /// document store and cannot be nested again: attempt to do so will result in an exception.
    #[new]
    pub fn new(dict: &PyDict) -> PyResult<Self> {
        let mut map: HashMap<String, PyObject> = HashMap::new();
        for (k, v) in dict.iter() {
            let k = k.downcast::<pyo3::types::PyString>()?.to_string();
            let v: PyObject = v.into();
            map.insert(k, v);
        }
        Ok(YMap(SharedType::Prelim(map)))
    }

    /// Returns true if this is a preliminary instance of `YMap`.
    ///
    /// Preliminary instances can be nested into other shared data types such as `YArray` and `YMap`.
    /// Once a preliminary instance has been inserted this way, it becomes integrated into y-py
    /// document store and cannot be nested again: attempt to do so will result in an exception.
    #[getter]
    pub fn prelim(&self) -> bool {
        match &self.0 {
            SharedType::Prelim(_) => true,
            _ => false,
        }
    }

    /// Returns a number of entries stored within this instance of `YMap`.
    pub fn length(&self, txn: &YTransaction) -> u32 {
        match &self.0 {
            SharedType::Integrated(v) => v.len(txn),
            SharedType::Prelim(v) => v.len() as u32,
        }
    }

    /// Converts contents of this `YMap` instance into a JSON representation.
    pub fn to_json(&self, txn: &YTransaction) -> PyResult<PyObject> {
        Python::with_gil(|py| match &self.0 {
            SharedType::Integrated(v) => Ok(v.to_json(txn).into_py(py)),
            SharedType::Prelim(v) => {
                let dict = PyDict::new(py);
                for (k, v) in v.iter() {
                    dict.set_item(k, v)?;
                }
                Ok(dict.into())
            }
        })
    }

    /// Sets a given `key`-`value` entry within this instance of `YMap`. If another entry was
    /// already stored under given `key`, it will be overridden with new `value`.
    pub fn set(&mut self, txn: &mut YTransaction, key: &str, value: PyObject) {
        match &mut self.0 {
            SharedType::Integrated(v) => {
                v.insert(txn, key.to_string(), PyValueWrapper(value));
            }
            SharedType::Prelim(v) => {
                v.insert(key.to_string(), value);
            }
        }
    }

    /// Removes an entry identified by a given `key` from this instance of `YMap`, if such exists.
    pub fn delete(&mut self, txn: &mut YTransaction, key: &str) {
        match &mut self.0 {
            SharedType::Integrated(v) => {
                v.remove(txn, key);
            }
            SharedType::Prelim(v) => {
                v.remove(key);
            }
        }
    }

    /// Returns value of an entry stored under given `key` within this instance of `YMap`,
    /// or `undefined` if no such entry existed.
    pub fn get(&self, txn: &mut YTransaction, key: &str) -> PyObject {
        match &self.0 {
            SharedType::Integrated(v) => Python::with_gil(|py| {
                if let Some(value) = v.get(txn, key) {
                    value.into_py(py)
                } else {
                    py.None()
                }
            }),
            SharedType::Prelim(v) => {
                if let Some(value) = v.get(key) {
                    value.clone()
                } else {
                    Python::with_gil(|py| py.None())
                }
            }
        }
    }

    /// Returns an iterator that can be used to traverse over all entries stored within this
    /// instance of `YMap`. Order of entry is not specified.
    ///
    /// Example:
    ///
    /// ```python
    /// from y_py import YDoc
    ///
    /// # document on machine A
    /// doc = YDoc()
    /// map = doc.get_map('name')
    /// with doc.begin_transaction() as txn:
    ///     map.set(txn, 'key1', 'value1')
    ///     map.set(txn, 'key2', true)
    ///     for (key, value) in map.entries(txn)):
    ///         print(key, value)
    /// ```
    pub fn entries(&self, txn: &mut YTransaction) -> YMapIterator {
        match &self.0 {
            SharedType::Integrated(val) => unsafe {
                let this: *const Map = val;
                let tx: *const Transaction = &txn.0 as *const _;
                let shared_iter =
                    SharedYMapIterator::Integrated((*this).iter(tx.as_ref().unwrap()));
                YMapIterator(ManuallyDrop::new(shared_iter))
            },
            SharedType::Prelim(val) => unsafe {
                let this: *const HashMap<String, PyObject> = val;
                let shared_iter = SharedYMapIterator::Prelim((*this).iter());
                YMapIterator(ManuallyDrop::new(shared_iter))
            },
        }
    }
}

pub enum SharedYMapIterator {
    Integrated(MapIter<'static>),
    Prelim(std::collections::hash_map::Iter<'static, String, PyObject>),
}

#[pyclass(unsendable)]
pub struct YMapIterator(ManuallyDrop<SharedYMapIterator>);

impl Drop for YMapIterator {
    fn drop(&mut self) {
        unsafe { ManuallyDrop::drop(&mut self.0) }
    }
}

#[pyproto]
impl<'p> PyIterProtocol for YMapIterator {
    fn __iter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }
    fn __next__(mut slf: PyRefMut<Self>) -> Option<(String, PyObject)> {
        match slf.0.deref_mut() {
            SharedYMapIterator::Integrated(iter) => {
                Python::with_gil(|py| iter.next().map(|(k, v)| (k.to_string(), v.into_py(py))))
            }
            SharedYMapIterator::Prelim(iter) => iter.next().map(|(k, v)| (k.clone(), v.clone())),
        }
    }
}

/// Event generated by `YMap.observe` method. Emitted during transaction commit phase.
#[pyclass(unsendable)]
pub struct YMapEvent {
    inner: *const MapEvent,
    txn: *const Transaction,
    target: Option<PyObject>,
    keys: Option<PyObject>,
}

impl YMapEvent {
    fn new(event: &MapEvent, txn: &Transaction) -> Self {
        let inner = event as *const MapEvent;
        let txn = txn as *const Transaction;
        YMapEvent {
            inner,
            txn,
            target: None,
            keys: None,
        }
    }

    fn inner(&self) -> &MapEvent {
        unsafe { self.inner.as_ref().unwrap() }
    }

    fn txn(&self) -> &Transaction {
        unsafe { self.txn.as_ref().unwrap() }
    }
}

#[pymethods]
impl YMapEvent {
    /// Returns a current shared type instance, that current event changes refer to.
    #[getter]
    pub fn target(&mut self) -> PyObject {
        if let Some(target) = self.target.as_ref() {
            target.clone()
        } else {
            let target: PyObject =
                Python::with_gil(|py| YMap::from(self.inner().target().clone()).into_py(py));
            self.target = Some(target.clone());
            target
        }
    }

    /// Returns an array of keys and indexes creating a path from root type down to current instance
    /// of shared type (accessible via `target` getter).
    pub fn path(&self) -> PyObject {
        Python::with_gil(|py| self.inner().path(self.txn()).into_py(py))
    }

    /// Returns a list of key-value changes made over corresponding `YMap` collection within
    /// bounds of current transaction. These changes follow a format:
    ///
    /// - { action: 'add'|'update'|'delete', oldValue: any|undefined, newValue: any|undefined }
    #[getter]
    pub fn keys(&mut self) -> PyObject {
        if let Some(keys) = &self.keys {
            keys.clone()
        } else {
            let keys: PyObject = Python::with_gil(|py| {
                let keys = self.inner().keys(self.txn());
                let result = PyDict::new(py);
                for (key, value) in keys.iter() {
                    let key = &**key;
                    result.set_item(key, value.into_py(py)).unwrap();
                }
                result.into()
            });

            self.keys = Some(keys.clone());
            keys
        }
    }
}
