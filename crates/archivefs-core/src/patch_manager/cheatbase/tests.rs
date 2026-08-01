use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::patch_manager::{
    CHEAT_SOURCE_RESULT_SCHEMA_VERSION, CheatSourceError, CheatSourceErrorStage,
    CheatSourceHttpResponse,
};

static FIXTURE_ID: AtomicUsize = AtomicUsize::new(0);

fn fixture_root(label: &str) -> PathBuf {
    let id = FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "archivefs-cheatbase-{label}-{}-{id}",
        std::process::id()
    ))
}

fn create_fixture(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let connection = Connection::open(path).unwrap();
    connection.execute_batch(
        "CREATE TABLE SYSTEMS(systemID INTEGER PRIMARY KEY AUTOINCREMENT,systemName TEXT NOT NULL,systemShortName TEXT NOT NULL,systemHeaderSizeBytes INTEGER,systemHashless INTEGER,systemHeader INTEGER,systemSerial TEXT,systemOEID TEXT,lastModified DATETIME NOT NULL DEFAULT (datetime('now')));
         CREATE TABLE REGIONS(regionID INTEGER PRIMARY KEY AUTOINCREMENT,regionName TEXT NOT NULL,lastModified DATETIME NOT NULL DEFAULT (datetime('now')));
         CREATE TABLE ROMS(romID INTEGER PRIMARY KEY AUTOINCREMENT,systemID INTEGER NOT NULL,regionID INTEGER NOT NULL,romHashCRC TEXT,romHashMD5 TEXT,romHashSHA1 TEXT,romSize INTEGER,romFileName TEXT NOT NULL,romExtensionlessFileName TEXT NOT NULL,romParent TEXT,romSerial TEXT,romHeader TEXT,romLanguage TEXT,romDumpSource TEXT NOT NULL,lastModified DATETIME NOT NULL DEFAULT (datetime('now')),FOREIGN KEY(systemID) REFERENCES SYSTEMS(systemID) ON UPDATE CASCADE ON DELETE RESTRICT,FOREIGN KEY(regionID) REFERENCES REGIONS(regionID) ON UPDATE CASCADE ON DELETE RESTRICT);
         CREATE TABLE RELEASES(releaseID INTEGER PRIMARY KEY AUTOINCREMENT,romID INTEGER NOT NULL,releaseTitleName TEXT NOT NULL,regionLocalizedID INTEGER NOT NULL,releaseCoverFront TEXT,releaseCoverBack TEXT,releaseCoverCart TEXT,releaseCoverDisc TEXT,releaseDescription TEXT,releaseDeveloper TEXT,releasePublisher TEXT,releaseGenre TEXT,releaseDate TEXT,releaseReferenceURL TEXT,releaseReferenceImageURL TEXT,lastModified DATETIME NOT NULL DEFAULT (datetime('now')),FOREIGN KEY(romID) REFERENCES ROMS(romID) ON UPDATE CASCADE ON DELETE RESTRICT);
         CREATE TABLE CHEAT_DEVICES(cheatDeviceID INTEGER PRIMARY KEY AUTOINCREMENT,systemID INTEGER NOT NULL,cheatDeviceName TEXT NOT NULL,cheatDeviceBrandName TEXT,cheatDeviceFormat TEXT,lastModified DATETIME NOT NULL DEFAULT (datetime('now')),FOREIGN KEY(systemID) REFERENCES SYSTEMS(systemID) ON UPDATE CASCADE ON DELETE RESTRICT);
         CREATE TABLE CHEAT_CATEGORIES(cheatCategoryID INTEGER PRIMARY KEY AUTOINCREMENT,cheatCategory TEXT NOT NULL,cheatCategoryDescription TEXT,lastModified DATETIME NOT NULL DEFAULT (datetime('now')));
         CREATE TABLE CHEATS(cheatID INTEGER PRIMARY KEY AUTOINCREMENT,romID INTEGER NOT NULL,cheatName TEXT NOT NULL,cheatActivation TEXT,cheatDescription TEXT,cheatSideEffect TEXT,cheatFolderName TEXT,cheatCategoryID INTEGER NOT NULL,cheatCode TEXT NOT NULL,cheatDeviceID INTEGER NOT NULL,cheatCredit TEXT,lastModified DATETIME NOT NULL DEFAULT (datetime('now')),FOREIGN KEY(romID) REFERENCES ROMS(romID) ON UPDATE CASCADE ON DELETE RESTRICT,FOREIGN KEY(cheatCategoryID) REFERENCES CHEAT_CATEGORIES(cheatCategoryID) ON UPDATE CASCADE ON DELETE RESTRICT,FOREIGN KEY(cheatDeviceID) REFERENCES CHEAT_DEVICES(cheatDeviceID) ON UPDATE CASCADE ON DELETE RESTRICT);
         CREATE TRIGGER update_SYSTEMS_lastModified_trigger AFTER UPDATE ON SYSTEMS BEGIN UPDATE SYSTEMS SET lastModified=datetime('now') WHERE systemID=NEW.systemID; END;
         CREATE TRIGGER update_REGIONS_lastModified_trigger AFTER UPDATE ON REGIONS BEGIN UPDATE REGIONS SET lastModified=datetime('now') WHERE regionID=NEW.regionID; END;
         CREATE TRIGGER update_ROMS_lastModified_trigger AFTER UPDATE ON ROMS BEGIN UPDATE ROMS SET lastModified=datetime('now') WHERE romID=NEW.romID; END;
         CREATE TRIGGER update_RELEASES_lastModified_trigger AFTER UPDATE ON RELEASES BEGIN UPDATE RELEASES SET lastModified=datetime('now') WHERE releaseID=NEW.releaseID; END;
         CREATE TRIGGER update_CHEAT_DEVICES_lastModified_trigger AFTER UPDATE ON CHEAT_DEVICES BEGIN UPDATE CHEAT_DEVICES SET lastModified=datetime('now') WHERE cheatDeviceID=NEW.cheatDeviceID; END;
         CREATE TRIGGER update_CHEAT_CATEGORIES_lastModified_trigger AFTER UPDATE ON CHEAT_CATEGORIES BEGIN UPDATE CHEAT_CATEGORIES SET lastModified=datetime('now') WHERE cheatCategoryID=NEW.cheatCategoryID; END;
         CREATE TRIGGER update_CHEATS_lastModified_trigger AFTER UPDATE ON CHEATS BEGIN UPDATE CHEATS SET lastModified=datetime('now') WHERE cheatID=NEW.cheatID; END;"
    ).unwrap();
    for id in 1..=43 {
        let name = verified_system_name(id).unwrap();
        connection
            .execute(
                "INSERT INTO SYSTEMS(systemID,systemName,systemShortName) VALUES(?,?,?)",
                params![id, name, format!("S{id}")],
            )
            .unwrap();
    }
    for id in 1..=39 {
        connection
            .execute(
                "INSERT INTO REGIONS(regionID,regionName) VALUES(?,?)",
                params![
                    id,
                    if id == 21 {
                        "USA".to_string()
                    } else {
                        format!("Region {id}")
                    }
                ],
            )
            .unwrap();
    }
    for id in 1..=24 {
        connection.execute("INSERT INTO CHEAT_DEVICES(cheatDeviceID,systemID,cheatDeviceName,cheatDeviceFormat) VALUES(?,?,?,?)", params![id,if id==10{24}else{18},if id==10{"Action Replay DS".to_string()}else{format!("Device {id}")},"XXXXXXXX YYYYYYYY"]).unwrap();
    }
    for id in 1..=45 {
        connection.execute("INSERT INTO CHEAT_CATEGORIES(cheatCategoryID,cheatCategory,cheatCategoryDescription) VALUES(?,?,?)", params![id,format!("Category {id}"),"Description"]).unwrap();
    }
    connection.execute("INSERT INTO ROMS(romID,systemID,regionID,romHashCRC,romHashMD5,romHashSHA1,romSize,romFileName,romExtensionlessFileName,romSerial,romDumpSource) VALUES(100,24,21,'1234ABCD','00112233445566778899AABBCCDDEEFF','00112233445566778899AABBCCDDEEFF00112233',1024,'Fixture.nds','Fixture','ABCE','Synthetic')", []).unwrap();
    connection.execute("INSERT INTO RELEASES(releaseID,romID,releaseTitleName,regionLocalizedID,releaseDate) VALUES(200,100,'Fixture Game',21,'2001'),(201,100,'Fixture Game',21,'2001')", []).unwrap();
    connection.execute("INSERT INTO CHEATS(cheatID,romID,cheatName,cheatDescription,cheatCategoryID,cheatCode,cheatDeviceID,cheatCredit) VALUES(300,100,'Infinite test','Synthetic fixture',1,'12345678 9ABCDEF0',10,'Tester'),(301,100,'Second page',NULL,2,'11111111 22222222',10,NULL)", []).unwrap();
}

fn cleanup(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

#[test]
fn real_shape_schema_is_accepted_without_source_indexes() {
    let root = fixture_root("schema");
    let database = root.join("source.sqlite");
    create_fixture(&database);
    let validation = validate_database_with_hash(&database, None).unwrap();
    assert_eq!(validation.counts.systems, 43);
    assert_eq!(validation.counts.cheats, 2);
    assert!(validation.opened_read_only && validation.immutable && validation.query_only);
    cleanup(&root);
}

#[test]
fn arbitrary_or_missing_schema_is_rejected() {
    let root = fixture_root("wrong");
    fs::create_dir_all(&root).unwrap();
    let database = root.join("wrong.sqlite");
    Connection::open(&database)
        .unwrap()
        .execute("CREATE TABLE unrelated(value TEXT)", [])
        .unwrap();
    assert_eq!(
        validate_database_with_hash(&database, None)
            .unwrap_err()
            .kind,
        CheatBaseErrorKind::UnsupportedSchema
    );
    cleanup(&root);
}

#[test]
fn exact_hash_lookup_preserves_ambiguity_between_releases() {
    let root = fixture_root("hash");
    let database = root.join("source.sqlite");
    create_fixture(&database);
    let catalogue = CheatBaseCatalogue::open_with_expected_hash(&database, None).unwrap();
    let result = catalogue
        .lookup_hash(
            CheatBaseHashAlgorithm::Sha1,
            "00112233445566778899aabbccddeeff00112233",
            Some("Nintendo DS"),
            PageRequest::games(0),
        )
        .unwrap();
    assert_eq!(result.confidence, ProviderGameMatchConfidence::Ambiguous);
    assert_eq!(result.page.total, 2);
    assert!(
        result
            .page
            .rows
            .iter()
            .all(|row| row.sha1.as_deref() == Some("00112233445566778899AABBCCDDEEFF00112233"))
    );
    cleanup(&root);
}

#[test]
fn malformed_hash_is_never_trusted() {
    let root = fixture_root("bad-hash");
    let database = root.join("source.sqlite");
    create_fixture(&database);
    let catalogue = CheatBaseCatalogue::open_with_expected_hash(&database, None).unwrap();
    assert_eq!(
        catalogue
            .lookup_hash(
                CheatBaseHashAlgorithm::Sha1,
                "not-a-hash",
                None,
                PageRequest::games(0)
            )
            .unwrap_err()
            .kind,
        CheatBaseErrorKind::InvalidIdentity
    );
    cleanup(&root);
}

#[test]
fn serial_platform_region_lookup_is_exact_but_not_first_result_guessing() {
    let root = fixture_root("serial");
    let database = root.join("source.sqlite");
    create_fixture(&database);
    let catalogue = CheatBaseCatalogue::open_with_expected_hash(&database, None).unwrap();
    let result = catalogue
        .lookup_serial("ABCE", "Nintendo DS", Some("USA"), PageRequest::games(0))
        .unwrap();
    assert_eq!(result.confidence, ProviderGameMatchConfidence::Ambiguous);
    assert_eq!(result.page.total, 2);
    cleanup(&root);
}

#[test]
fn title_search_and_code_pages_are_bounded_and_browse_only() {
    let root = fixture_root("browse");
    let database = root.join("source.sqlite");
    create_fixture(&database);
    let catalogue = CheatBaseCatalogue::open_with_expected_hash(&database, None).unwrap();
    let result = catalogue
        .search_games(&CheatBaseGameSearchRequest {
            platform_id: Some("Nintendo DS".to_string()),
            title: "Fixture Game".to_string(),
            region: Some("USA".to_string()),
            upstream_release_id: None,
            page: PageRequest::games(0),
        })
        .unwrap();
    assert_eq!(result.confidence, ProviderGameMatchConfidence::Ambiguous);
    let cheats = catalogue
        .cheats(
            200,
            PageRequest {
                offset: 0,
                limit: u16::MAX,
            },
        )
        .unwrap();
    assert_eq!(cheats.limit, PageRequest::HARD_LIMIT);
    assert_eq!(cheats.total, 2);
    assert!(
        cheats
            .rows
            .iter()
            .all(|row| row.device.compatibility != DeviceFormatCompatibility::DirectlyInstallable)
    );
    cleanup(&root);
}

#[test]
fn every_known_mapping_target_is_canonical_and_unknown_stays_visible() {
    for id in 1..=43 {
        let name = verified_system_name(id).unwrap();
        let mapping = cheatbase_platform_mapping(id, name);
        if let Some(target) = mapping.archivefs_platform_id.as_deref() {
            assert!(
                crate::platform::platform_by_id(target).is_some(),
                "{id} -> {target}"
            );
        }
    }
    let unknown = cheatbase_platform_mapping(999, "Future Console");
    assert_eq!(unknown.status, PlatformMappingStatus::Unknown);
    assert_eq!(unknown.upstream_name, "Future Console");
}

#[test]
fn stage_one_has_no_directly_installable_device() {
    for id in 1..=24 {
        assert_ne!(
            cheatbase_device_mapping(id, &format!("Device {id}")).compatibility,
            DeviceFormatCompatibility::DirectlyInstallable
        );
    }
}

#[test]
fn immutable_queries_create_no_sqlite_sidecars_or_source_changes() {
    let root = fixture_root("immutable");
    let database = root.join("source.sqlite");
    create_fixture(&database);
    let before = fingerprint_regular_file(&database).unwrap();
    let before_meta = fs::metadata(&database).unwrap();
    let catalogue = CheatBaseCatalogue::open_with_expected_hash(&database, None).unwrap();
    let _ = catalogue.systems(PageRequest::games(0)).unwrap();
    drop(catalogue);
    let after = fingerprint_regular_file(&database).unwrap();
    let after_meta = fs::metadata(&database).unwrap();
    assert_eq!(before, after);
    assert_eq!(before_meta.len(), after_meta.len());
    assert!(!database.with_extension("sqlite-journal").exists());
    assert!(!PathBuf::from(format!("{}-wal", database.display())).exists());
    assert!(!PathBuf::from(format!("{}-shm", database.display())).exists());
    cleanup(&root);
}

#[test]
fn local_import_copies_without_modifying_original() {
    let root = fixture_root("import");
    let source = root.join("selected.sqlite");
    create_fixture(&source);
    let before = fingerprint_regular_file(&source).unwrap();
    let before_mtime = fs::metadata(&source).unwrap().modified().unwrap();
    let paths = CheatBasePaths::at(root.join("owned"));
    let result = import_local_with_expected_hash(&paths, &source, None).unwrap();
    assert!(result.status.usable);
    assert!(!result.network_used);
    assert_eq!(before, fingerprint_regular_file(&source).unwrap());
    assert_eq!(
        before_mtime,
        fs::metadata(&source).unwrap().modified().unwrap()
    );
    cleanup(&root);
}

#[test]
fn failed_replacement_preserves_known_good_database() {
    let root = fixture_root("replace");
    let source = root.join("selected.sqlite");
    create_fixture(&source);
    let paths = CheatBasePaths::at(root.join("owned"));
    import_local_with_expected_hash(&paths, &source, None).unwrap();
    let before = fingerprint_regular_file(&paths.database).unwrap();
    let bad = root.join("bad.sqlite");
    fs::write(&bad, b"not sqlite").unwrap();
    assert!(import_local_with_expected_hash(&paths, &bad, None).is_err());
    assert_eq!(before, fingerprint_regular_file(&paths.database).unwrap());
    cleanup(&root);
}

#[test]
fn status_json_states_nintendo_ds_only_coverage_and_browse_only_safety() {
    let root = fixture_root("status-coverage");
    let source = root.join("selected.sqlite");
    create_fixture(&source);
    let paths = CheatBasePaths::at(root.join("owned"));
    import_local_with_expected_hash(&paths, &source, None).unwrap();
    let status = inspect_cheatbase_source(&paths).unwrap();
    assert_eq!(status.cheat_coverage_platforms, ["Nintendo DS"]);
    assert_eq!(status.identity_metadata_platforms.len(), 38);
    assert!(status.browse_only);
    assert!(!status.install_supported);
    assert_eq!(
        status.licence_status,
        CheatProviderLicenceStatus::NotEstablished
    );
    assert_eq!(status.source_fingerprint, status.fingerprint);
    let json = serde_json::to_value(status).unwrap();
    for field in [
        "cheat_coverage_platforms",
        "identity_metadata_platforms",
        "browse_only",
        "install_supported",
        "licence_status",
        "provenance",
        "source_fingerprint",
    ] {
        assert!(json.get(field).is_some(), "missing status field {field}");
    }
    cleanup(&root);
}

#[test]
fn non_ds_results_are_identity_only_while_ds_names_action_replay() {
    let root = fixture_root("coverage-results");
    let database = root.join("source.sqlite");
    create_fixture(&database);
    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "INSERT INTO ROMS(romID,systemID,regionID,romFileName,romExtensionlessFileName,romDumpSource) VALUES(101,25,21,'Identity.nes','Identity','Synthetic')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO RELEASES(releaseID,romID,releaseTitleName,regionLocalizedID) VALUES(202,101,'Identity Only Game',21)",
            [],
        )
        .unwrap();
    drop(connection);
    let catalogue = CheatBaseCatalogue::open_with_expected_hash(&database, None).unwrap();

    let non_ds = catalogue
        .search_games(&CheatBaseGameSearchRequest {
            platform_id: Some("NES".to_string()),
            title: "Identity Only Game".to_string(),
            region: None,
            upstream_release_id: None,
            page: PageRequest::games(0),
        })
        .unwrap();
    assert!(
        non_ds
            .explanation
            .contains("identity-metadata records only")
    );
    let game = &non_ds.page.rows[0];
    assert_eq!(game.cheat_count, None);
    assert!(!game.platform_has_cheat_coverage);
    assert!(game.cheat_coverage_note.contains("no cheat coverage"));

    let ds = catalogue.game(200).unwrap().unwrap();
    assert_eq!(ds.cheat_count, Some(2));
    assert!(ds.platform_has_cheat_coverage);
    assert_eq!(ds.cheat_device_formats, ["Action Replay DS"]);
    assert!(ds.cheat_coverage_note.contains("Nintendo DS only"));
    assert!(ds.cheat_coverage_note.contains("Action Replay DS"));

    let systems = catalogue
        .systems(PageRequest {
            offset: 0,
            limit: 50,
        })
        .unwrap();
    let nes = systems
        .rows
        .iter()
        .find(|system| system.upstream_id == 25)
        .unwrap();
    assert_eq!(nes.cheat_count, None);
    assert!(nes.cheat_coverage_note.contains("Identity metadata only"));
    let devices = catalogue
        .devices(PageRequest {
            offset: 0,
            limit: 50,
        })
        .unwrap();
    let action_replay = devices
        .rows
        .iter()
        .find(|device| device.upstream_id == 10)
        .unwrap();
    assert!(action_replay.contains_cheats);
    assert!(action_replay.coverage_note.contains("Action Replay DS"));
    cleanup(&root);
}

#[test]
fn malformed_and_truncated_sqlite_headers_are_rejected() {
    let root = fixture_root("headers");
    fs::create_dir_all(&root).unwrap();
    for (name, bytes) in [
        ("short", b"SQLite".as_slice()),
        ("wrong", b"not a sqlite database".as_slice()),
    ] {
        let path = root.join(name);
        fs::write(&path, bytes).unwrap();
        assert_eq!(
            validate_database_with_hash(&path, None).unwrap_err().kind,
            CheatBaseErrorKind::NotSqlite
        );
    }
    cleanup(&root);
}

#[test]
fn missing_column_and_broken_relationships_are_rejected() {
    let root = fixture_root("integrity");
    let missing = root.join("missing.sqlite");
    create_fixture(&missing);
    Connection::open(&missing)
        .unwrap()
        .execute("ALTER TABLE CHEATS DROP COLUMN cheatCredit", [])
        .unwrap();
    assert_eq!(
        validate_database_with_hash(&missing, None)
            .unwrap_err()
            .kind,
        CheatBaseErrorKind::UnsupportedSchema
    );

    let orphaned = root.join("orphaned.sqlite");
    create_fixture(&orphaned);
    let connection = Connection::open(&orphaned).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    connection
        .execute("UPDATE CHEATS SET romID=999999 WHERE cheatID=300", [])
        .unwrap();
    drop(connection);
    assert_eq!(
        validate_database_with_hash(&orphaned, None)
            .unwrap_err()
            .kind,
        CheatBaseErrorKind::Validation
    );
    cleanup(&root);
}

#[test]
fn oversized_local_source_is_refused_before_copying() {
    let root = fixture_root("oversize");
    fs::create_dir_all(&root).unwrap();
    let source = root.join("oversized.sqlite");
    File::create(&source)
        .unwrap()
        .set_len(CHEATBASE_MAX_DATABASE_BYTES + 1)
        .unwrap();
    let paths = CheatBasePaths::at(root.join("owned"));
    assert_eq!(
        import_local_with_expected_hash(&paths, &source, None)
            .unwrap_err()
            .kind,
        CheatBaseErrorKind::DownloadTooLarge
    );
    assert!(!paths.database.exists());
    cleanup(&root);
}

#[test]
fn oversized_text_is_bounded_on_a_utf8_boundary_and_reported() {
    let root = fixture_root("bounded-text");
    let database = root.join("source.sqlite");
    create_fixture(&database);
    let oversized = "é".repeat((MAX_CODE_BODY / 2) + 20);
    Connection::open(&database)
        .unwrap()
        .execute(
            "UPDATE CHEATS SET cheatCode=? WHERE cheatID=300",
            params![oversized],
        )
        .unwrap();
    let catalogue = CheatBaseCatalogue::open_with_expected_hash(&database, None).unwrap();
    let page = catalogue.cheats(200, PageRequest::cheats(0)).unwrap();
    let row = page.rows.iter().find(|row| row.upstream_id == 300).unwrap();
    assert!(row.code.len() <= MAX_CODE_BODY);
    assert!(row.truncated_fields.iter().any(|field| field == "code"));
    assert!(row.code.is_char_boundary(row.code.len()));
    cleanup(&root);
}

#[derive(Clone, Copy)]
enum FakeDownloadResult {
    Redirect,
    Cancelled,
}

struct FakeTransport(FakeDownloadResult);

impl CheatSourceTransport for FakeTransport {
    fn get(
        &self,
        _url: &str,
        _maximum_bytes: u64,
        _destination: &mut dyn Write,
        _context: CheatSourceTransferContext<'_>,
    ) -> Result<CheatSourceHttpResponse, CheatSourceError> {
        match self.0 {
            FakeDownloadResult::Redirect => Ok(CheatSourceHttpResponse {
                status: 302,
                content_type: None,
                content_encoding: None,
                content_length: None,
                location: Some("https://example.invalid/untrusted".to_string()),
                etag: None,
                last_modified: None,
                downloaded_bytes: 0,
                retry_after_seconds: None,
            }),
            FakeDownloadResult::Cancelled => Err(CheatSourceError {
                schema_version: CHEAT_SOURCE_RESULT_SCHEMA_VERSION,
                stage: CheatSourceErrorStage::Download,
                code: "cancelled".to_string(),
                message: "cancelled".to_string(),
                retry_after_seconds: None,
            }),
        }
    }
}

#[test]
fn redirects_and_cancellation_leave_no_activated_source() {
    for (label, transport, expected) in [
        (
            "redirect",
            FakeTransport(FakeDownloadResult::Redirect),
            CheatBaseErrorKind::RedirectRejected,
        ),
        (
            "cancelled",
            FakeTransport(FakeDownloadResult::Cancelled),
            CheatBaseErrorKind::Cancelled,
        ),
    ] {
        let root = fixture_root(label);
        let paths = CheatBasePaths::at(root.join("owned"));
        assert_eq!(
            download_cheatbase_database(&paths, &CheatBaseDownloadOptions::default(), &transport)
                .unwrap_err()
                .kind,
            expected
        );
        assert!(!paths.database.exists());
        cleanup(&root);
    }
}

#[test]
fn provider_adds_no_application_migration() {
    assert_eq!(crate::latest_schema_version(), 6);
    let migration_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/migrations");
    let mut migrations = fs::read_dir(migration_root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    migrations.sort();
    assert_eq!(
        migrations,
        [
            "0001_initial.sql",
            "0002_platform_aliases.sql",
            "0003_source_folder_scan_status.sql",
            "0004_scan_skip_counts.sql",
            "0005_source_platform_assignment.sql",
            "0006_game_identity_reports.sql",
        ]
    );
}
