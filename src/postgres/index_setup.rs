use crate::index::indexing::IndexingOptions;
use crate::index::optimizing::OptimizingOptions;
use crate::index::segments::SegmentsOptions;
use crate::index::{IndexOptions, VectorOptions};
use crate::postgres::datatype::VectorTypmod;
use crate::prelude::*;
use serde::{Deserialize, Serialize};
use std::ffi::CStr;
use validator::Validate;

pub fn helper_offset() -> usize {
    memoffset::offset_of!(Helper, offset)
}

pub fn helper_size() -> usize {
    std::mem::size_of::<Helper>()
}

pub unsafe fn convert_opclass_to_distance(opclass: pgrx::pg_sys::Oid) -> Distance {
    let opclass_cache_id = pgrx::pg_sys::SysCacheIdentifier_CLAOID as _;
    let tuple = pgrx::pg_sys::SearchSysCache1(opclass_cache_id, opclass.into());
    assert!(
        !tuple.is_null(),
        "cache lookup failed for operator class {opclass}"
    );
    let classform = pgrx::pg_sys::GETSTRUCT(tuple).cast::<pgrx::pg_sys::FormData_pg_opclass>();
    let opfamily = (*classform).opcfamily;
    let distance = convert_opfamily_to_distance(opfamily);
    pgrx::pg_sys::ReleaseSysCache(tuple);
    distance
}

pub unsafe fn convert_opfamily_to_distance(opfamily: pgrx::pg_sys::Oid) -> Distance {
    let opfamily_cache_id = pgrx::pg_sys::SysCacheIdentifier_OPFAMILYOID as _;
    let opstrategy_cache_id = pgrx::pg_sys::SysCacheIdentifier_AMOPSTRATEGY as _;
    let tuple = pgrx::pg_sys::SearchSysCache1(opfamily_cache_id, opfamily.into());
    assert!(
        !tuple.is_null(),
        "cache lookup failed for operator family {opfamily}"
    );
    let list = pgrx::pg_sys::SearchSysCacheList(
        opstrategy_cache_id,
        1,
        opfamily.into(),
        0.into(),
        0.into(),
    );
    assert!((*list).n_members == 1);
    let member = (*list).members.as_slice(1)[0];
    let member_tuple = &mut (*member).tuple;
    let amop = pgrx::pg_sys::GETSTRUCT(member_tuple).cast::<pgrx::pg_sys::FormData_pg_amop>();
    assert!((*amop).amopstrategy == 1);
    assert!((*amop).amoppurpose == pgrx::pg_sys::AMOP_ORDER as libc::c_char);
    let operator = (*amop).amopopr;
    let distance;
    if operator == regoperatorin("<->(vector,vector)") {
        distance = Distance::L2;
    } else if operator == regoperatorin("<#>(vector,vector)") {
        distance = Distance::Dot;
    } else if operator == regoperatorin("<=>(vector,vector)") {
        distance = Distance::Cosine;
    } else {
        FriendlyError::UnsupportedOperator.friendly();
    };
    pgrx::pg_sys::ReleaseCatCacheList(list);
    pgrx::pg_sys::ReleaseSysCache(tuple);
    distance
}

pub unsafe fn options(index_relation: pgrx::pg_sys::Relation) -> IndexOptions {
    let nkeysatts = (*(*index_relation).rd_index).indnkeyatts;
    assert!(nkeysatts == 1, "Can not be built on multicolumns.");
    // get distance
    let opfamily = (*index_relation).rd_opfamily.read();
    let d = convert_opfamily_to_distance(opfamily);
    // get dims
    let attrs = (*(*index_relation).rd_att).attrs.as_slice(1);
    let attr = &attrs[0];
    let typmod = VectorTypmod::parse_from_i32(attr.type_mod()).unwrap();
    let dims = typmod.dims().ok_or(FriendlyError::DimsIsNeeded).friendly();
    // get other options
    let parsed = get_parsed_from_varlena((*index_relation).rd_options);
    let options = IndexOptions {
        vector: VectorOptions { dims, d },
        segment: parsed.segment,
        optimizing: parsed.optimizing,
        indexing: parsed.indexing,
    };
    if let Err(errors) = options.validate() {
        FriendlyError::BadOption(errors.to_string()).friendly();
    }
    options
}

#[derive(Copy, Clone, Debug)]
#[repr(C)]
struct Helper {
    pub vl_len_: i32,
    pub offset: i32,
}

unsafe fn get_parsed_from_varlena(helper: *const pgrx::pg_sys::varlena) -> Parsed {
    let helper = helper as *const Helper;
    if helper.is_null() || (*helper).offset == 0 {
        return Default::default();
    }
    let ptr = (helper as *const libc::c_char).offset((*helper).offset as isize);
    let cstr = CStr::from_ptr(ptr);
    toml::from_str::<Parsed>(cstr.to_str().unwrap()).unwrap()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Parsed {
    #[serde(default)]
    segment: SegmentsOptions,
    #[serde(default)]
    optimizing: OptimizingOptions,
    #[serde(default)]
    indexing: IndexingOptions,
}

fn regoperatorin(name: &str) -> pgrx::pg_sys::Oid {
    use pgrx::IntoDatum;
    let cstr = std::ffi::CString::new(name).expect("specified name has embedded NULL byte");
    unsafe {
        pgrx::direct_function_call::<pgrx::pg_sys::Oid>(
            pgrx::pg_sys::regoperatorin,
            &[cstr.as_c_str().into_datum()],
        )
        .expect("operator lookup returned NULL")
    }
}
