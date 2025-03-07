// Copyright 2020 TiKV Project Authors. Licensed under Apache-2.0.

//! Engines for use in the test suite, implementing both the KvEngine
//! and RaftEngine traits.
//!
//! These engines link to all other engines, providing concrete single storage
//! engine type to run tests against.
//!
//! This provides a simple way to integrate non-RocksDB engines into the
//! existing test suite without too much disruption.
//!
//! Engines presently supported by this crate are
//!
//! - RocksEngine from engine_rocks
//! - PanicEngine from engine_panic
//! - RaftLogEngine from raft_log_engine
//!
//! TiKV uses two different storage engine instances,
//! the "raft" engine, for storing consensus data,
//! and the "kv" engine, for storing user data.
//!
//! The types and constructors for these two engines are located in the `raft`
//! and `kv` modules respectively.
//!
//! The engine for each module is chosen at compile time with feature flags:
//!
//! - `--features test-engine-kv-rocksdb`
//! - `--features test-engine-kv-panic`
//! - `--features test-engine-raft-rocksdb`
//! - `--features test-engine-raft-panic`
//! - `--features test-engine-raft-raft-engine`
//!
//! By default, the `tikv` crate turns on `test-engine-kv-rocksdb`,
//! and `test-engine-raft-raft-engine`. This behavior can be disabled
//! with `--disable-default-features`.
//!
//! The `tikv` crate additionally provides some feature flags that
//! contral both the `kv` and `raft` engines at the same time:
//!
//! - `--features test-engines-rocksdb`
//! - `--features test-engines-panic`
//!
//! So, e.g., to run the test suite with the panic engine:
//!
//! ```
//! cargo test --all --disable-default-features --features=protobuf_codec,test-engines-panic
//! ```
//!
//! We'll probably revisit the engine-testing strategy in the future,
//! e.g. by using engine-parameterized tests instead.
//!
//! This create also contains a `ctor` module that contains constructor methods
//! appropriate for constructing storage engines of any type. It is intended
//! that this module is _the only_ module within TiKV that knows about concrete
//! storage engines, and that it be extracted into its own crate for use in
//! TiKV, once the full requirements are better understood.

/// Types and constructors for the "raft" engine
pub mod raft {
    #[cfg(feature = "test-engine-raft-panic")]
    pub use engine_panic::PanicEngine as RaftTestEngine;
    #[cfg(feature = "test-engine-raft-rocksdb")]
    pub use engine_rocks::RocksEngine as RaftTestEngine;
    use engine_traits::Result;
    #[cfg(feature = "test-engine-raft-raft-engine")]
    pub use raft_log_engine::RaftLogEngine as RaftTestEngine;

    use crate::ctor::{RaftDBOptions, RaftEngineConstructorExt};

    pub fn new_engine(path: &str, db_opt: Option<RaftDBOptions>) -> Result<RaftTestEngine> {
        RaftTestEngine::new_raft_engine(path, db_opt)
    }
}

/// Types and constructors for the "kv" engine
pub mod kv {
    use std::{
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
    };

    use collections::HashMap;
    #[cfg(feature = "test-engine-kv-panic")]
    pub use engine_panic::{
        PanicEngine as KvTestEngine, PanicEngineIterator as KvTestEngineIterator,
        PanicSnapshot as KvTestSnapshot, PanicWriteBatch as KvTestWriteBatch,
    };
    #[cfg(feature = "test-engine-kv-rocksdb")]
    pub use engine_rocks::{
        RocksEngine as KvTestEngine, RocksEngineIterator as KvTestEngineIterator,
        RocksSnapshot as KvTestSnapshot, RocksWriteBatchVec as KvTestWriteBatch,
    };
    use engine_traits::{
        CFOptionsExt, ColumnFamilyOptions, Result, TabletAccessor, TabletFactory, CF_DEFAULT,
    };
    use tikv_util::box_err;

    use crate::ctor::{CFOptions, DBOptions, KvEngineConstructorExt};

    pub fn new_engine(
        path: &str,
        db_opt: Option<DBOptions>,
        cfs: &[&str],
        opts: Option<Vec<CFOptions>>,
    ) -> Result<KvTestEngine> {
        KvTestEngine::new_kv_engine(path, db_opt, cfs, opts)
    }

    pub fn new_engine_opt(
        path: &str,
        db_opt: DBOptions,
        cfs_opts: Vec<CFOptions>,
    ) -> Result<KvTestEngine> {
        KvTestEngine::new_kv_engine_opt(path, db_opt, cfs_opts)
    }

    const TOMBSTONE_MARK: &str = "TOMBSTONE_TABLET";

    #[derive(Clone)]
    pub struct TestTabletFactory {
        root_path: String,
        db_opt: Option<DBOptions>,
        cfs: Vec<String>,
        opts: Option<Vec<CFOptions>>,
        registry: Arc<Mutex<HashMap<(u64, u64), KvTestEngine>>>,
    }

    impl TestTabletFactory {
        pub fn new(
            root_path: &str,
            db_opt: Option<DBOptions>,
            cfs: &[&str],
            opts: Option<Vec<CFOptions>>,
        ) -> Self {
            Self {
                root_path: root_path.to_string(),
                db_opt,
                cfs: cfs.iter().map(|s| s.to_string()).collect(),
                opts,
                registry: Arc::new(Mutex::new(HashMap::default())),
            }
        }
    }

    // Extract tablet id and tablet suffix from the path.
    fn get_id_and_suffix_from_path(path: &Path) -> (u64, u64) {
        let (mut tablet_id, mut tablet_suffix) = (0, 1);
        if let Some(s) = path.file_name().map(|s| s.to_string_lossy()) {
            let mut split = s.split('_');
            tablet_id = split.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            tablet_suffix = split.next().and_then(|s| s.parse().ok()).unwrap_or(1);
        }
        (tablet_id, tablet_suffix)
    }

    impl TabletFactory<KvTestEngine> for TestTabletFactory {
        fn create_tablet(&self, id: u64, suffix: u64) -> Result<KvTestEngine> {
            let mut reg = self.registry.lock().unwrap();
            if let Some(db) = reg.get(&(id, suffix)) {
                return Err(box_err!(
                    "region {} {} already exists",
                    id,
                    db.as_inner().path()
                ));
            }
            let tablet_path = self.tablet_path(id, suffix);
            let tablet_path = tablet_path.to_str().unwrap();
            let mut cfs = vec![];
            self.cfs.iter().for_each(|s| cfs.push(s.as_str()));
            let kv_engine = KvTestEngine::new_kv_engine(
                tablet_path,
                self.db_opt.clone(),
                cfs.as_slice(),
                self.opts.clone(),
            )?;
            reg.insert((id, suffix), kv_engine.clone());
            Ok(kv_engine)
        }

        fn open_tablet(&self, id: u64, suffix: u64) -> Result<KvTestEngine> {
            let mut reg = self.registry.lock().unwrap();
            if let Some(db) = reg.get(&(id, suffix)) {
                return Ok(db.clone());
            }

            let db_path = self.tablet_path(id, suffix);
            let db = self.open_tablet_raw(db_path.as_path(), false)?;
            reg.insert((id, suffix), db.clone());
            Ok(db)
        }

        fn open_tablet_cache(&self, id: u64, suffix: u64) -> Option<KvTestEngine> {
            let reg = self.registry.lock().unwrap();
            if let Some(db) = reg.get(&(id, suffix)) {
                return Some(db.clone());
            }
            None
        }

        fn open_tablet_cache_any(&self, id: u64) -> Option<KvTestEngine> {
            let reg = self.registry.lock().unwrap();
            if let Some(k) = reg.keys().find(|k| k.0 == id) {
                return Some(reg.get(k).unwrap().clone());
            }
            None
        }

        fn open_tablet_raw(&self, path: &Path, _readonly: bool) -> Result<KvTestEngine> {
            if !KvTestEngine::exists(path.to_str().unwrap_or_default()) {
                return Err(box_err!(
                    "path {} does not have db",
                    path.to_str().unwrap_or_default()
                ));
            }
            let (tablet_id, tablet_suffix) = get_id_and_suffix_from_path(path);
            self.create_tablet(tablet_id, tablet_suffix)
        }

        #[inline]
        fn create_shared_db(&self) -> Result<KvTestEngine> {
            self.create_tablet(0, 0)
        }

        #[inline]
        fn exists_raw(&self, path: &Path) -> bool {
            KvTestEngine::exists(path.to_str().unwrap_or_default())
        }

        #[inline]
        fn tablets_path(&self) -> PathBuf {
            Path::new(&self.root_path).join("tablets")
        }

        #[inline]
        fn tablet_path(&self, id: u64, suffix: u64) -> PathBuf {
            Path::new(&self.root_path).join(format!("tablets/{}_{}", id, suffix))
        }

        #[inline]
        fn mark_tombstone(&self, region_id: u64, suffix: u64) {
            let path = self.tablet_path(region_id, suffix).join(TOMBSTONE_MARK);
            std::fs::File::create(&path).unwrap();
            self.registry.lock().unwrap().remove(&(region_id, suffix));
        }

        #[inline]
        fn is_tombstoned(&self, region_id: u64, suffix: u64) -> bool {
            self.tablet_path(region_id, suffix)
                .join(TOMBSTONE_MARK)
                .exists()
        }

        #[inline]
        fn destroy_tablet(&self, id: u64, suffix: u64) -> engine_traits::Result<()> {
            let path = self.tablet_path(id, suffix);
            self.registry.lock().unwrap().remove(&(id, suffix));
            let _ = std::fs::remove_dir_all(path);
            Ok(())
        }

        #[inline]
        fn load_tablet(&self, path: &Path, id: u64, suffix: u64) -> Result<KvTestEngine> {
            {
                let reg = self.registry.lock().unwrap();
                if let Some(db) = reg.get(&(id, suffix)) {
                    return Err(box_err!(
                        "region {} {} already exists",
                        id,
                        db.as_inner().path()
                    ));
                }
            }

            let db_path = self.tablet_path(id, suffix);
            std::fs::rename(path, &db_path)?;
            let new_engine = self.open_tablet_raw(db_path.as_path(), false);
            if new_engine.is_ok() {
                let (old_id, old_suffix) = get_id_and_suffix_from_path(path);
                self.registry.lock().unwrap().remove(&(old_id, old_suffix));
            }
            new_engine
        }

        fn set_shared_block_cache_capacity(
            &self,
            capacity: u64,
        ) -> std::result::Result<(), String> {
            let reg = self.registry.lock().unwrap();
            // pick up any tablet and set the shared block cache capacity
            if let Some(((_id, _suffix), tablet)) = (*reg).iter().next() {
                let opt = tablet.get_options_cf(CF_DEFAULT).unwrap(); // FIXME unwrap
                opt.set_block_cache_capacity(capacity)?;
            }
            Ok(())
        }
    }

    impl TabletAccessor<KvTestEngine> for TestTabletFactory {
        #[inline]
        fn for_each_opened_tablet(&self, f: &mut dyn FnMut(u64, u64, &KvTestEngine)) {
            let reg = self.registry.lock().unwrap();
            for ((id, suffix), tablet) in &*reg {
                f(*id, *suffix, tablet)
            }
        }

        // it have multi tablets.
        fn is_single_engine(&self) -> bool {
            false
        }
    }
}

/// Create a storage engine with a concrete type. This should ultimately be the
/// only module within TiKV that needs to know about concrete engines. Other
/// code only uses the `engine_traits` abstractions.
///
/// At the moment this has a lot of open-coding of engine-specific
/// initialization, but in the future more constructor abstractions should be
/// pushed down into engine_traits.
///
/// This module itself is intended to be extracted from this crate into its own
/// crate, once the requirements for engine construction are better understood.
pub mod ctor {
    use std::sync::Arc;

    use encryption::DataKeyManager;
    use engine_traits::Result;
    use file_system::IORateLimiter;

    /// Kv engine construction
    ///
    /// For simplicity, all engine constructors are expected to configure every
    /// engine such that all of TiKV and its tests work correctly, for the
    /// constructed column families.
    ///
    /// Specifically, this means that RocksDB constructors should set up
    /// all properties collectors, always.
    pub trait KvEngineConstructorExt: Sized {
        /// Create a new kv engine with either:
        ///
        /// - The column families specified as `cfs`, with default options, or
        /// - The column families specified as `opts`, with options.
        ///
        /// Note that if `opts` is not `None` then the `cfs` argument is completely ignored.
        ///
        /// The engine stores its data in the `path` directory.
        /// If that directory does not exist, then it is created.
        fn new_kv_engine(
            path: &str,
            db_opt: Option<DBOptions>,
            cfs: &[&str],
            opts: Option<Vec<CFOptions>>,
        ) -> Result<Self>;

        /// Create a new engine with specified column families and options
        ///
        /// The engine stores its data in the `path` directory.
        /// If that directory does not exist, then it is created.
        fn new_kv_engine_opt(
            path: &str,
            db_opt: DBOptions,
            cfs_opts: Vec<CFOptions>,
        ) -> Result<Self>;
    }

    /// Raft engine construction
    pub trait RaftEngineConstructorExt: Sized {
        /// Create a new raft engine.
        fn new_raft_engine(path: &str, db_opt: Option<RaftDBOptions>) -> Result<Self>;
    }

    #[derive(Clone, Default)]
    pub struct DBOptions {
        key_manager: Option<Arc<DataKeyManager>>,
        rate_limiter: Option<Arc<IORateLimiter>>,
        enable_multi_batch_write: bool,
    }

    impl DBOptions {
        pub fn set_key_manager(&mut self, key_manager: Option<Arc<DataKeyManager>>) {
            self.key_manager = key_manager;
        }

        pub fn set_rate_limiter(&mut self, rate_limiter: Option<Arc<IORateLimiter>>) {
            self.rate_limiter = rate_limiter;
        }

        pub fn set_enable_multi_batch_write(&mut self, enable: bool) {
            self.enable_multi_batch_write = enable;
        }
    }

    pub type RaftDBOptions = DBOptions;

    #[derive(Clone)]
    pub struct CFOptions {
        pub cf: String,
        pub options: ColumnFamilyOptions,
    }

    impl CFOptions {
        pub fn new(cf: &str, options: ColumnFamilyOptions) -> CFOptions {
            CFOptions {
                cf: cf.to_string(),
                options,
            }
        }
    }

    /// Properties for a single column family
    ///
    /// All engines must emulate column families, but at present it is not clear
    /// how non-RocksDB engines should deal with the wide variety of options for
    /// column families.
    ///
    /// At present this very closely mirrors the column family options
    /// for RocksDB, with the exception that it provides no capacity for
    /// installing table property collectors, which have little hope of being
    /// emulated on arbitrary engines.
    ///
    /// Instead, the RocksDB constructors need to always install the table
    /// property collectors that TiKV needs, and other engines need to
    /// accomplish the same high-level ends those table properties are used for
    /// by their own means.
    ///
    /// At present, they should probably emulate, reinterpret, or ignore them as
    /// suitable to get tikv functioning.
    ///
    /// In the future TiKV will probably have engine-specific configuration
    /// options.
    #[derive(Clone)]
    pub struct ColumnFamilyOptions {
        disable_auto_compactions: bool,
        level_zero_file_num_compaction_trigger: Option<i32>,
        level_zero_slowdown_writes_trigger: Option<i32>,
        /// On RocksDB, turns off the range properties collector. Only used in
        /// tests. Unclear how other engines should deal with this.
        no_range_properties: bool,
        /// On RocksDB, turns off the table properties collector. Only used in
        /// tests. Unclear how other engines should deal with this.
        no_table_properties: bool,
    }

    impl ColumnFamilyOptions {
        pub fn new() -> ColumnFamilyOptions {
            ColumnFamilyOptions {
                disable_auto_compactions: false,
                level_zero_file_num_compaction_trigger: None,
                level_zero_slowdown_writes_trigger: None,
                no_range_properties: false,
                no_table_properties: false,
            }
        }

        pub fn set_disable_auto_compactions(&mut self, v: bool) {
            self.disable_auto_compactions = v;
        }

        pub fn get_disable_auto_compactions(&self) -> bool {
            self.disable_auto_compactions
        }

        pub fn set_level_zero_file_num_compaction_trigger(&mut self, n: i32) {
            self.level_zero_file_num_compaction_trigger = Some(n);
        }

        pub fn get_level_zero_file_num_compaction_trigger(&self) -> Option<i32> {
            self.level_zero_file_num_compaction_trigger
        }

        pub fn set_level_zero_slowdown_writes_trigger(&mut self, n: i32) {
            self.level_zero_slowdown_writes_trigger = Some(n);
        }

        pub fn get_level_zero_slowdown_writes_trigger(&self) -> Option<i32> {
            self.level_zero_slowdown_writes_trigger
        }

        pub fn set_no_range_properties(&mut self, v: bool) {
            self.no_range_properties = v;
        }

        pub fn get_no_range_properties(&self) -> bool {
            self.no_range_properties
        }

        pub fn set_no_table_properties(&mut self, v: bool) {
            self.no_table_properties = v;
        }

        pub fn get_no_table_properties(&self) -> bool {
            self.no_table_properties
        }
    }

    impl Default for ColumnFamilyOptions {
        fn default() -> Self {
            Self::new()
        }
    }

    mod panic {
        use engine_panic::PanicEngine;
        use engine_traits::Result;

        use super::{CFOptions, DBOptions, KvEngineConstructorExt, RaftEngineConstructorExt};

        impl KvEngineConstructorExt for engine_panic::PanicEngine {
            fn new_kv_engine(
                _path: &str,
                _db_opt: Option<DBOptions>,
                _cfs: &[&str],
                _opts: Option<Vec<CFOptions>>,
            ) -> Result<Self> {
                Ok(PanicEngine)
            }

            fn new_kv_engine_opt(
                _path: &str,
                _db_opt: DBOptions,
                _cfs_opts: Vec<CFOptions>,
            ) -> Result<Self> {
                Ok(PanicEngine)
            }
        }

        impl RaftEngineConstructorExt for engine_panic::PanicEngine {
            fn new_raft_engine(_path: &str, _db_opt: Option<DBOptions>) -> Result<Self> {
                Ok(PanicEngine)
            }
        }
    }

    mod rocks {
        use engine_rocks::{
            get_env,
            properties::{MvccPropertiesCollectorFactory, RangePropertiesCollectorFactory},
            raw::{
                ColumnFamilyOptions as RawRocksColumnFamilyOptions, DBOptions as RawRocksDBOptions,
            },
            util::{
                new_engine as rocks_new_engine, new_engine_opt as rocks_new_engine_opt,
                RocksCFOptions,
            },
            RocksColumnFamilyOptions, RocksDBOptions,
        };
        use engine_traits::{ColumnFamilyOptions as ColumnFamilyOptionsTrait, Result};

        use super::{
            CFOptions, ColumnFamilyOptions, DBOptions, KvEngineConstructorExt, RaftDBOptions,
            RaftEngineConstructorExt,
        };

        impl KvEngineConstructorExt for engine_rocks::RocksEngine {
            // FIXME this is duplicating behavior from engine_rocks::raw_util in order to
            // call set_standard_cf_opts.
            fn new_kv_engine(
                path: &str,
                db_opt: Option<DBOptions>,
                cfs: &[&str],
                opts: Option<Vec<CFOptions>>,
            ) -> Result<Self> {
                let rocks_db_opts = match db_opt {
                    Some(db_opt) => Some(get_rocks_db_opts(db_opt)?),
                    None => None,
                };
                let cfs_opts = match opts {
                    Some(opts) => opts,
                    None => {
                        let mut default_cfs_opts = Vec::with_capacity(cfs.len());
                        for cf in cfs {
                            default_cfs_opts.push(CFOptions::new(*cf, ColumnFamilyOptions::new()));
                        }
                        default_cfs_opts
                    }
                };
                let rocks_cfs_opts = cfs_opts
                    .iter()
                    .map(|cf_opts| {
                        let mut rocks_cf_opts = RocksColumnFamilyOptions::new();
                        set_standard_cf_opts(rocks_cf_opts.as_raw_mut(), &cf_opts.options);
                        set_cf_opts(&mut rocks_cf_opts, &cf_opts.options);
                        RocksCFOptions::new(&cf_opts.cf, rocks_cf_opts)
                    })
                    .collect();
                rocks_new_engine(path, rocks_db_opts, &[], Some(rocks_cfs_opts))
            }

            fn new_kv_engine_opt(
                path: &str,
                db_opt: DBOptions,
                cfs_opts: Vec<CFOptions>,
            ) -> Result<Self> {
                let rocks_db_opts = get_rocks_db_opts(db_opt)?;
                let rocks_cfs_opts = cfs_opts
                    .iter()
                    .map(|cf_opts| {
                        let mut rocks_cf_opts = RocksColumnFamilyOptions::new();
                        set_standard_cf_opts(rocks_cf_opts.as_raw_mut(), &cf_opts.options);
                        set_cf_opts(&mut rocks_cf_opts, &cf_opts.options);
                        RocksCFOptions::new(&cf_opts.cf, rocks_cf_opts)
                    })
                    .collect();
                rocks_new_engine_opt(path, rocks_db_opts, rocks_cfs_opts)
            }
        }

        impl RaftEngineConstructorExt for engine_rocks::RocksEngine {
            fn new_raft_engine(path: &str, db_opt: Option<RaftDBOptions>) -> Result<Self> {
                let rocks_db_opts = match db_opt {
                    Some(db_opt) => Some(get_rocks_db_opts(db_opt)?),
                    None => None,
                };
                let cf_opts = CFOptions::new(engine_traits::CF_DEFAULT, ColumnFamilyOptions::new());
                let mut rocks_cf_opts = RocksColumnFamilyOptions::new();
                set_standard_cf_opts(rocks_cf_opts.as_raw_mut(), &cf_opts.options);
                set_cf_opts(&mut rocks_cf_opts, &cf_opts.options);
                let default_cfs_opts = vec![RocksCFOptions::new(&cf_opts.cf, rocks_cf_opts)];
                rocks_new_engine(path, rocks_db_opts, &[], Some(default_cfs_opts))
            }
        }

        fn set_standard_cf_opts(
            rocks_cf_opts: &mut RawRocksColumnFamilyOptions,
            cf_opts: &ColumnFamilyOptions,
        ) {
            if !cf_opts.get_no_range_properties() {
                rocks_cf_opts.add_table_properties_collector_factory(
                    "tikv.range-properties-collector",
                    RangePropertiesCollectorFactory::default(),
                );
            }
            if !cf_opts.get_no_table_properties() {
                rocks_cf_opts.add_table_properties_collector_factory(
                    "tikv.mvcc-properties-collector",
                    MvccPropertiesCollectorFactory::default(),
                );
            }
        }

        fn set_cf_opts(
            rocks_cf_opts: &mut RocksColumnFamilyOptions,
            cf_opts: &ColumnFamilyOptions,
        ) {
            if let Some(trigger) = cf_opts.get_level_zero_file_num_compaction_trigger() {
                rocks_cf_opts.set_level_zero_file_num_compaction_trigger(trigger);
            }
            if let Some(trigger) = cf_opts.get_level_zero_slowdown_writes_trigger() {
                rocks_cf_opts
                    .as_raw_mut()
                    .set_level_zero_slowdown_writes_trigger(trigger);
            }
            if cf_opts.get_disable_auto_compactions() {
                rocks_cf_opts.set_disable_auto_compactions(true);
            }
        }

        fn get_rocks_db_opts(db_opts: DBOptions) -> Result<RocksDBOptions> {
            let mut rocks_db_opts = RawRocksDBOptions::new();
            let env = get_env(db_opts.key_manager.clone(), db_opts.rate_limiter)?;
            rocks_db_opts.set_env(env);
            if db_opts.enable_multi_batch_write {
                rocks_db_opts.enable_unordered_write(false);
                rocks_db_opts.enable_pipelined_write(false);
                rocks_db_opts.enable_multi_batch_write(true);
            }
            let rocks_db_opts = RocksDBOptions::from_raw(rocks_db_opts);
            Ok(rocks_db_opts)
        }
    }

    mod raft_engine {
        use engine_traits::Result;
        use raft_log_engine::{RaftEngineConfig, RaftLogEngine};

        use super::{RaftDBOptions, RaftEngineConstructorExt};

        impl RaftEngineConstructorExt for raft_log_engine::RaftLogEngine {
            fn new_raft_engine(path: &str, db_opts: Option<RaftDBOptions>) -> Result<Self> {
                let mut config = RaftEngineConfig::default();
                config.dir = path.to_owned();
                RaftLogEngine::new(
                    config,
                    db_opts.as_ref().and_then(|opts| opts.key_manager.clone()),
                    db_opts.and_then(|opts| opts.rate_limiter),
                )
            }
        }
    }
}

/// Create a new set of engines in a temporary directory
///
/// This is little-used and probably shouldn't exist.
pub fn new_temp_engine(
    path: &tempfile::TempDir,
) -> engine_traits::Engines<crate::kv::KvTestEngine, crate::raft::RaftTestEngine> {
    let raft_path = path.path().join(std::path::Path::new("raft"));
    engine_traits::Engines::new(
        crate::kv::new_engine(
            path.path().to_str().unwrap(),
            None,
            engine_traits::ALL_CFS,
            None,
        )
        .unwrap(),
        crate::raft::new_engine(raft_path.to_str().unwrap(), None).unwrap(),
    )
}
