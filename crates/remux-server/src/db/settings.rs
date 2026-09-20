use anyhow::Result;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::api::{EncodingOptions, ServerConfiguration};
use remux_sdks::remux::IntroOptions;

const SERVER_CONFIG_KEY: &str = "server_configuration";
const ENCODING_CONFIG_KEY: &str = "encoding_configuration";
const INTRO_CONFIG_KEY: &str = "intro_configuration";
const SIMKL_CONFIG_KEY: &str = "simkl_configuration";

pub struct Settings;

impl Settings {
    pub async fn get_config(db: &SqlitePool) -> Result<ServerConfiguration> {
        match Self::get(db, SERVER_CONFIG_KEY).await? {
            Some(json) => Ok(serde_json::from_str(&json)?),
            None => Ok(ServerConfiguration::default()),
        }
    }

    pub async fn get_config_or_default(db: &SqlitePool) -> ServerConfiguration {
        match Self::get_config(db).await {
            Ok(config) => config,
            Err(err) => {
                tracing::error!("Failed to load server configuration: {err}");
                ServerConfiguration::default()
            }
        }
    }

    pub async fn set_config(
        db: &SqlitePool,
        config: &ServerConfiguration,
    ) -> Result<()> {
        let json = serde_json::to_string(config)?;
        Self::set(db, SERVER_CONFIG_KEY, &json).await
    }

    pub async fn get_encoding_config(db: &SqlitePool) -> Result<EncodingOptions> {
        Ok(match Self::get(db, ENCODING_CONFIG_KEY).await? {
            Some(json) => serde_json::from_str(&json).unwrap_or_default(),
            None => EncodingOptions::default(),
        })
    }

    pub async fn set_encoding_config(
        db: &SqlitePool,
        opts: &EncodingOptions,
    ) -> Result<()> {
        let json = serde_json::to_string(opts)?;
        Self::set(db, ENCODING_CONFIG_KEY, &json).await
    }

    pub async fn get_intro_config(db: &SqlitePool) -> Result<IntroOptions> {
        Ok(match Self::get(db, INTRO_CONFIG_KEY).await? {
            Some(json) => serde_json::from_str(&json).unwrap_or_default(),
            None => IntroOptions::default(),
        })
    }

    pub async fn set_intro_config(db: &SqlitePool, opts: &IntroOptions) -> Result<()> {
        let json = serde_json::to_string(opts)?;
        Self::set(db, INTRO_CONFIG_KEY, &json).await
    }

    pub async fn get_simkl_config(
        db: &SqlitePool,
        base_config: &crate::SimklConfig,
    ) -> Result<crate::SimklConfig> {
        let mut cfg = base_config.clone();
        if let Some(json) = Self::get(db, SIMKL_CONFIG_KEY).await? {
            if let Ok(db_cfg) = serde_json::from_str::<crate::SimklConfig>(&json) {
                if !db_cfg
                    .client_id
                    .is_empty()
                {
                    cfg.client_id = db_cfg.client_id;
                }
                if db_cfg.completion_threshold > 0 {
                    cfg.completion_threshold = db_cfg.completion_threshold;
                }
                for (uid, user_cfg) in db_cfg.users {
                    cfg.users
                        .insert(uid, user_cfg);
                }
            }
        }
        Ok(cfg)
    }

    pub async fn set_simkl_config(
        db: &SqlitePool,
        cfg: &crate::SimklConfig,
    ) -> Result<()> {
        let json = serde_json::to_string(cfg)?;
        Self::set(db, SIMKL_CONFIG_KEY, &json).await
    }

    pub async fn get_user_simkl_config(
        db: &SqlitePool,
        base_config: &crate::SimklConfig,
        user_id: &Uuid,
    ) -> Result<crate::SimklUserConfig> {
        let full_cfg = Self::get_simkl_config(db, base_config).await?;
        let key = user_id.to_string();
        let simple_key = user_id
            .simple()
            .to_string();
        Ok(full_cfg
            .users
            .get(&key)
            .or_else(|| {
                full_cfg
                    .users
                    .get(&simple_key)
            })
            .cloned()
            .unwrap_or_default())
    }

    pub async fn set_user_simkl_config(
        db: &SqlitePool,
        base_config: &crate::SimklConfig,
        user_id: &Uuid,
        user_config: crate::SimklUserConfig,
    ) -> Result<()> {
        let mut full_cfg = Self::get_simkl_config(db, base_config).await?;
        let key = user_id.to_string();
        full_cfg
            .users
            .insert(key, user_config);
        Self::set_simkl_config(db, &full_cfg).await
    }

    pub async fn init_server_id(db: &SqlitePool) -> Result<()> {
        let id = match Self::get(db, "server_id").await? {
            Some(existing) => Uuid::parse_str(&existing)
                .map(|u| {
                    u.simple()
                        .to_string()
                })
                .unwrap_or(existing),
            None => {
                let new_id = Uuid::new_v4()
                    .simple()
                    .to_string();
                Self::set(db, "server_id", &new_id).await?;
                new_id
            }
        };
        crate::common::set_server_id(id);
        Ok(())
    }

    pub async fn get(db: &SqlitePool, key: &str) -> Result<Option<String>> {
        let row = sqlx::query_scalar::<_, String>(
            "SELECT value FROM settings WHERE key = ?1",
        )
        .bind(key)
        .fetch_optional(db)
        .await?;
        Ok(row)
    }

    pub async fn set(db: &SqlitePool, key: &str, value: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(db)
        .await?;
        Ok(())
    }
}
