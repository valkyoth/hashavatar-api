use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use aws_config::{BehaviorVersion, Region};
use aws_sdk_s3::Client as S3Client;
use aws_sdk_s3::config::Builder as S3ConfigBuilder;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::ServerSideEncryption;
use axum::extract::{ConnectInfo, Extension, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use hashavatar::{
    AVATAR_STYLE_VERSION, AvatarAccessory, AvatarBackground, AvatarColor, AvatarExpression,
    AvatarIdentityOptions, AvatarKind, AvatarNamespace, AvatarOptions, AvatarOutputFormat,
    AvatarShape, AvatarSpec, AvatarStyleOptions, encode_avatar_style_with_identity_options,
    render_avatar_for_namespace,
};
use image::{GenericImage, ImageBuffer, Rgba, RgbaImage};
use ipnet::IpNet;
use lru::LruCache;
use opentelemetry::metrics::{Counter, Histogram};
use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, Semaphore};

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8080;
const TRUSTED_PROXIES_ENV: &str = "HASHAVATAR_TRUSTED_PROXIES";
const DEFAULT_ID: &str = "cat@hashavatar.app";
const SITE_NAME: &str = "hashavatar.app";
const SITE_URL: &str = "https://hashavatar.app";
const REPOSITORY_URL: &str = "https://github.com/valkyoth/hashavatar-api";
const CRATE_URL: &str = "https://crates.io/crates/hashavatar/";
const DEFAULT_NAMESPACE_TENANT: &str = "public";
const DEFAULT_NAMESPACE_STYLE: &str = "v2";
const DEFAULT_HASH_ALGORITHM: &str = "sha512";
const DEFAULT_ACCESSORY: AvatarAccessory = AvatarAccessory::None;
const DEFAULT_COLOR: AvatarColor = AvatarColor::Default;
const DEFAULT_EXPRESSION: AvatarExpression = AvatarExpression::Default;
const DEFAULT_SHAPE: AvatarShape = AvatarShape::Square;
const AVATAR_TIMEOUT_MS: u64 = 3_000;
const STORAGE_TIMEOUT_MS: u64 = 5_000;
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);
const MAX_RATE_LIMIT_BUCKETS: usize = 65_536;
const RATE_LIMIT_SHARDS: usize = 64;
const MAX_CONCURRENT_RENDERS: usize = 64;
const INTERNAL_ERROR_MESSAGE: &str = "An internal server error occurred.";
const MIN_SIZE: u32 = 64;
const MAX_SIZE: u32 = 1024;
const MAX_ID_BYTES: usize = 512;
const MAX_NAMESPACE_COMPONENT_BYTES: usize = 64;
const DEFAULT_S3_PRESIGN_TTL_SECONDS: u64 = 900;
const MIN_S3_PRESIGN_TTL_SECONDS: u64 = 60;
const MAX_S3_PRESIGN_TTL_SECONDS: u64 = 604_800;
const PRESET_PAGE_SIZE: usize = 12;
const INVALID_NAMESPACE_MESSAGE: &str = "invalid namespace: tenant and style_version must be 1-64 ASCII letters, digits, hyphens, or underscores";
const INVALID_HASH_ALGORITHM_MESSAGE: &str = "unsupported hash algorithm: expected sha512";
const INVALID_AVATAR_FORMAT_MESSAGE: &str = "unsupported avatar format: expected webp";
const INVALID_AVATAR_RENDER_MESSAGE: &str = "avatar generation failed";
const INDEX_SCRIPT_SHA256: &str = "'sha256-7gjoUnTfcILxVkX3DugGXgaAEhWr+Pn91S0M+2HGQTs='";
const INDEX_SCRIPT_SHA256_COMPAT: &str = "'sha256-ZswfTY7H35rbv8WC7NXBoiC7WNu86vSzCDChNWwZZDM='";
const OTEL_SERVICE_NAME: &str = "hashavatar-api";
const COUNTRY_UNKNOWN: &str = "unknown";
const MAX_VISIBLE_SECONDS: u64 = 86_400;
const DEFAULT_LOCALE_ID: &str = "en-EU";
const LOCALES_TOML: &str = include_str!("../config/locales.toml");
const EN_EU_KEYS_TOML: &str = include_str!("../config/i18n/keys/en-EU.toml");
const EN_GB_KEYS_TOML: &str = include_str!("../config/i18n/keys/en-GB.toml");
const EN_US_KEYS_TOML: &str = include_str!("../config/i18n/keys/en-US.toml");
const FR_FR_KEYS_TOML: &str = include_str!("../config/i18n/keys/fr-FR.toml");
const DE_DE_KEYS_TOML: &str = include_str!("../config/i18n/keys/de-DE.toml");
const SV_SE_KEYS_TOML: &str = include_str!("../config/i18n/keys/sv-SE.toml");
const NB_NO_KEYS_TOML: &str = include_str!("../config/i18n/keys/nb-NO.toml");
const NL_NL_KEYS_TOML: &str = include_str!("../config/i18n/keys/nl-NL.toml");
const FI_FI_KEYS_TOML: &str = include_str!("../config/i18n/keys/fi-FI.toml");
const IS_IS_KEYS_TOML: &str = include_str!("../config/i18n/keys/is-IS.toml");
const ES_ES_KEYS_TOML: &str = include_str!("../config/i18n/keys/es-ES.toml");
const PT_PT_KEYS_TOML: &str = include_str!("../config/i18n/keys/pt-PT.toml");
const IT_IT_KEYS_TOML: &str = include_str!("../config/i18n/keys/it-IT.toml");
const JA_JP_KEYS_TOML: &str = include_str!("../config/i18n/keys/ja-JP.toml");
const ZH_CN_KEYS_TOML: &str = include_str!("../config/i18n/keys/zh-CN.toml");
const ZH_TW_KEYS_TOML: &str = include_str!("../config/i18n/keys/zh-TW.toml");
const VI_VN_KEYS_TOML: &str = include_str!("../config/i18n/keys/vi-VN.toml");
const TH_TH_KEYS_TOML: &str = include_str!("../config/i18n/keys/th-TH.toml");
const HI_IN_KEYS_TOML: &str = include_str!("../config/i18n/keys/hi-IN.toml");
const BN_IN_KEYS_TOML: &str = include_str!("../config/i18n/keys/bn-IN.toml");
const TA_IN_KEYS_TOML: &str = include_str!("../config/i18n/keys/ta-IN.toml");
const EO_001_KEYS_TOML: &str = include_str!("../config/i18n/keys/eo-001.toml");
const DA_DK_KEYS_TOML: &str = include_str!("../config/i18n/keys/da-DK.toml");
const LA_VA_KEYS_TOML: &str = include_str!("../config/i18n/keys/la-VA.toml");
const GSW_CH_KEYS_TOML: &str = include_str!("../config/i18n/keys/gsw-CH.toml");
const KO_KR_KEYS_TOML: &str = include_str!("../config/i18n/keys/ko-KR.toml");
const RU_RU_KEYS_TOML: &str = include_str!("../config/i18n/keys/ru-RU.toml");
const UK_UA_KEYS_TOML: &str = include_str!("../config/i18n/keys/uk-UA.toml");
const TR_TR_KEYS_TOML: &str = include_str!("../config/i18n/keys/tr-TR.toml");
const LT_LT_KEYS_TOML: &str = include_str!("../config/i18n/keys/lt-LT.toml");
const LV_LV_KEYS_TOML: &str = include_str!("../config/i18n/keys/lv-LV.toml");
const PL_PL_KEYS_TOML: &str = include_str!("../config/i18n/keys/pl-PL.toml");
const EL_GR_KEYS_TOML: &str = include_str!("../config/i18n/keys/el-GR.toml");
const HU_HU_KEYS_TOML: &str = include_str!("../config/i18n/keys/hu-HU.toml");
const ET_EE_KEYS_TOML: &str = include_str!("../config/i18n/keys/et-EE.toml");
const OVD_SE_KEYS_TOML: &str = include_str!("../config/i18n/keys/ovd-SE.toml");
const BG_BG_KEYS_TOML: &str = include_str!("../config/i18n/keys/bg-BG.toml");
const CS_CZ_KEYS_TOML: &str = include_str!("../config/i18n/keys/cs-CZ.toml");
const HR_HR_KEYS_TOML: &str = include_str!("../config/i18n/keys/hr-HR.toml");
const BE_BY_KEYS_TOML: &str = include_str!("../config/i18n/keys/be-BY.toml");
const GA_IE_KEYS_TOML: &str = include_str!("../config/i18n/keys/ga-IE.toml");
const LB_LU_KEYS_TOML: &str = include_str!("../config/i18n/keys/lb-LU.toml");
const RO_RO_KEYS_TOML: &str = include_str!("../config/i18n/keys/ro-RO.toml");
const SR_RS_KEYS_TOML: &str = include_str!("../config/i18n/keys/sr-RS.toml");
const NAP_IT_KEYS_TOML: &str = include_str!("../config/i18n/keys/nap-IT.toml");
const SK_SK_KEYS_TOML: &str = include_str!("../config/i18n/keys/sk-SK.toml");
const SL_SI_KEYS_TOML: &str = include_str!("../config/i18n/keys/sl-SI.toml");
const FY_NL_KEYS_TOML: &str = include_str!("../config/i18n/keys/fy-NL.toml");
const SE_NO_KEYS_TOML: &str = include_str!("../config/i18n/keys/se-NO.toml");
const SCN_IT_KEYS_TOML: &str = include_str!("../config/i18n/keys/scn-IT.toml");
const AR_001_KEYS_TOML: &str = include_str!("../config/i18n/keys/ar-001.toml");
const ID_ID_KEYS_TOML: &str = include_str!("../config/i18n/keys/id-ID.toml");
const UR_PK_KEYS_TOML: &str = include_str!("../config/i18n/keys/ur-PK.toml");
const MR_IN_KEYS_TOML: &str = include_str!("../config/i18n/keys/mr-IN.toml");
const JV_ID_KEYS_TOML: &str = include_str!("../config/i18n/keys/jv-ID.toml");
const MS_MY_KEYS_TOML: &str = include_str!("../config/i18n/keys/ms-MY.toml");
const FIL_PH_KEYS_TOML: &str = include_str!("../config/i18n/keys/fil-PH.toml");
const FA_IR_KEYS_TOML: &str = include_str!("../config/i18n/keys/fa-IR.toml");
const HE_IL_KEYS_TOML: &str = include_str!("../config/i18n/keys/he-IL.toml");
const SW_KE_KEYS_TOML: &str = include_str!("../config/i18n/keys/sw-KE.toml");
const PA_IN_KEYS_TOML: &str = include_str!("../config/i18n/keys/pa-IN.toml");
const TE_IN_KEYS_TOML: &str = include_str!("../config/i18n/keys/te-IN.toml");
const GU_IN_KEYS_TOML: &str = include_str!("../config/i18n/keys/gu-IN.toml");
const KN_IN_KEYS_TOML: &str = include_str!("../config/i18n/keys/kn-IN.toml");
const ML_IN_KEYS_TOML: &str = include_str!("../config/i18n/keys/ml-IN.toml");
const NE_NP_KEYS_TOML: &str = include_str!("../config/i18n/keys/ne-NP.toml");
const SI_LK_KEYS_TOML: &str = include_str!("../config/i18n/keys/si-LK.toml");
const MY_MM_KEYS_TOML: &str = include_str!("../config/i18n/keys/my-MM.toml");
const KM_KH_KEYS_TOML: &str = include_str!("../config/i18n/keys/km-KH.toml");
const LO_LA_KEYS_TOML: &str = include_str!("../config/i18n/keys/lo-LA.toml");
const MN_MN_KEYS_TOML: &str = include_str!("../config/i18n/keys/mn-MN.toml");
const HA_NG_KEYS_TOML: &str = include_str!("../config/i18n/keys/ha-NG.toml");
const YO_NG_KEYS_TOML: &str = include_str!("../config/i18n/keys/yo-NG.toml");
const IG_NG_KEYS_TOML: &str = include_str!("../config/i18n/keys/ig-NG.toml");
const AM_ET_KEYS_TOML: &str = include_str!("../config/i18n/keys/am-ET.toml");
const OM_ET_KEYS_TOML: &str = include_str!("../config/i18n/keys/om-ET.toml");
const SO_SO_KEYS_TOML: &str = include_str!("../config/i18n/keys/so-SO.toml");
const ZU_ZA_KEYS_TOML: &str = include_str!("../config/i18n/keys/zu-ZA.toml");
const AF_ZA_KEYS_TOML: &str = include_str!("../config/i18n/keys/af-ZA.toml");
const CA_ES_KEYS_TOML: &str = include_str!("../config/i18n/keys/ca-ES.toml");
const EU_ES_KEYS_TOML: &str = include_str!("../config/i18n/keys/eu-ES.toml");
const GL_ES_KEYS_TOML: &str = include_str!("../config/i18n/keys/gl-ES.toml");
const CY_GB_KEYS_TOML: &str = include_str!("../config/i18n/keys/cy-GB.toml");
const SQ_AL_KEYS_TOML: &str = include_str!("../config/i18n/keys/sq-AL.toml");
const BS_BA_KEYS_TOML: &str = include_str!("../config/i18n/keys/bs-BA.toml");
const MK_MK_KEYS_TOML: &str = include_str!("../config/i18n/keys/mk-MK.toml");
const MT_MT_KEYS_TOML: &str = include_str!("../config/i18n/keys/mt-MT.toml");
const HY_AM_KEYS_TOML: &str = include_str!("../config/i18n/keys/hy-AM.toml");
const KA_GE_KEYS_TOML: &str = include_str!("../config/i18n/keys/ka-GE.toml");
const AZ_AZ_KEYS_TOML: &str = include_str!("../config/i18n/keys/az-AZ.toml");
const KK_KZ_KEYS_TOML: &str = include_str!("../config/i18n/keys/kk-KZ.toml");
const UZ_UZ_KEYS_TOML: &str = include_str!("../config/i18n/keys/uz-UZ.toml");
const KY_KG_KEYS_TOML: &str = include_str!("../config/i18n/keys/ky-KG.toml");
const TG_TJ_KEYS_TOML: &str = include_str!("../config/i18n/keys/tg-TJ.toml");
const TK_TM_KEYS_TOML: &str = include_str!("../config/i18n/keys/tk-TM.toml");
const PS_AF_KEYS_TOML: &str = include_str!("../config/i18n/keys/ps-AF.toml");
const CKB_IQ_KEYS_TOML: &str = include_str!("../config/i18n/keys/ckb-IQ.toml");
const KU_TR_KEYS_TOML: &str = include_str!("../config/i18n/keys/ku-TR.toml");
const TI_ER_KEYS_TOML: &str = include_str!("../config/i18n/keys/ti-ER.toml");
const RW_RW_KEYS_TOML: &str = include_str!("../config/i18n/keys/rw-RW.toml");
const MG_MG_KEYS_TOML: &str = include_str!("../config/i18n/keys/mg-MG.toml");
const SN_ZW_KEYS_TOML: &str = include_str!("../config/i18n/keys/sn-ZW.toml");
const XH_ZA_KEYS_TOML: &str = include_str!("../config/i18n/keys/xh-ZA.toml");

static RENDER_SLOTS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_CONCURRENT_RENDERS)));

#[derive(Debug, Clone, Deserialize)]
struct LocaleConfig {
    default_locale: String,
    locales: Vec<Locale>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct Locale {
    locale_id: String,
    html_lang: String,
    dir: Option<String>,
    url_prefix: String,
    display_name: String,
    flag: String,
}

#[derive(Clone, Copy)]
struct I18n {
    locale: &'static Locale,
    keys: &'static toml::Value,
}

static LOCALE_CONFIG: LazyLock<LocaleConfig> = LazyLock::new(|| {
    let config = toml::from_str::<LocaleConfig>(LOCALES_TOML)
        .unwrap_or_else(|error| panic!("valid locale config TOML: {error}"));
    validate_locale_config(&config).unwrap_or_else(|error| panic!("valid locale config: {error}"));
    config
});

static EN_EU_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("en-EU", EN_EU_KEYS_TOML));
static EN_GB_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("en-GB", EN_GB_KEYS_TOML));
static EN_US_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("en-US", EN_US_KEYS_TOML));
static FR_FR_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("fr-FR", FR_FR_KEYS_TOML));
static DE_DE_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("de-DE", DE_DE_KEYS_TOML));
static SV_SE_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("sv-SE", SV_SE_KEYS_TOML));
static NB_NO_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("nb-NO", NB_NO_KEYS_TOML));
static NL_NL_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("nl-NL", NL_NL_KEYS_TOML));
static FI_FI_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("fi-FI", FI_FI_KEYS_TOML));
static IS_IS_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("is-IS", IS_IS_KEYS_TOML));
static ES_ES_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("es-ES", ES_ES_KEYS_TOML));
static PT_PT_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("pt-PT", PT_PT_KEYS_TOML));
static IT_IT_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("it-IT", IT_IT_KEYS_TOML));
static JA_JP_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ja-JP", JA_JP_KEYS_TOML));
static ZH_CN_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("zh-CN", ZH_CN_KEYS_TOML));
static ZH_TW_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("zh-TW", ZH_TW_KEYS_TOML));
static VI_VN_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("vi-VN", VI_VN_KEYS_TOML));
static TH_TH_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("th-TH", TH_TH_KEYS_TOML));
static HI_IN_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("hi-IN", HI_IN_KEYS_TOML));
static BN_IN_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("bn-IN", BN_IN_KEYS_TOML));
static TA_IN_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ta-IN", TA_IN_KEYS_TOML));
static EO_001_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("eo-001", EO_001_KEYS_TOML));
static DA_DK_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("da-DK", DA_DK_KEYS_TOML));
static LA_VA_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("la-VA", LA_VA_KEYS_TOML));
static GSW_CH_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("gsw-CH", GSW_CH_KEYS_TOML));
static KO_KR_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ko-KR", KO_KR_KEYS_TOML));
static RU_RU_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ru-RU", RU_RU_KEYS_TOML));
static UK_UA_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("uk-UA", UK_UA_KEYS_TOML));
static TR_TR_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("tr-TR", TR_TR_KEYS_TOML));
static LT_LT_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("lt-LT", LT_LT_KEYS_TOML));
static LV_LV_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("lv-LV", LV_LV_KEYS_TOML));
static PL_PL_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("pl-PL", PL_PL_KEYS_TOML));
static EL_GR_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("el-GR", EL_GR_KEYS_TOML));
static HU_HU_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("hu-HU", HU_HU_KEYS_TOML));
static ET_EE_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("et-EE", ET_EE_KEYS_TOML));
static OVD_SE_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ovd-SE", OVD_SE_KEYS_TOML));
static BG_BG_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("bg-BG", BG_BG_KEYS_TOML));
static CS_CZ_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("cs-CZ", CS_CZ_KEYS_TOML));
static HR_HR_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("hr-HR", HR_HR_KEYS_TOML));
static BE_BY_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("be-BY", BE_BY_KEYS_TOML));
static GA_IE_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ga-IE", GA_IE_KEYS_TOML));
static LB_LU_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("lb-LU", LB_LU_KEYS_TOML));
static RO_RO_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ro-RO", RO_RO_KEYS_TOML));
static SR_RS_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("sr-RS", SR_RS_KEYS_TOML));
static NAP_IT_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("nap-IT", NAP_IT_KEYS_TOML));
static SK_SK_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("sk-SK", SK_SK_KEYS_TOML));
static SL_SI_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("sl-SI", SL_SI_KEYS_TOML));
static FY_NL_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("fy-NL", FY_NL_KEYS_TOML));
static SE_NO_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("se-NO", SE_NO_KEYS_TOML));
static SCN_IT_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("scn-IT", SCN_IT_KEYS_TOML));
static AR_001_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ar-001", AR_001_KEYS_TOML));
static ID_ID_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("id-ID", ID_ID_KEYS_TOML));
static UR_PK_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ur-PK", UR_PK_KEYS_TOML));
static MR_IN_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("mr-IN", MR_IN_KEYS_TOML));
static JV_ID_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("jv-ID", JV_ID_KEYS_TOML));
static MS_MY_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ms-MY", MS_MY_KEYS_TOML));
static FIL_PH_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("fil-PH", FIL_PH_KEYS_TOML));
static FA_IR_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("fa-IR", FA_IR_KEYS_TOML));
static HE_IL_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("he-IL", HE_IL_KEYS_TOML));
static SW_KE_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("sw-KE", SW_KE_KEYS_TOML));
static PA_IN_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("pa-IN", PA_IN_KEYS_TOML));
static TE_IN_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("te-IN", TE_IN_KEYS_TOML));
static GU_IN_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("gu-IN", GU_IN_KEYS_TOML));
static KN_IN_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("kn-IN", KN_IN_KEYS_TOML));
static ML_IN_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ml-IN", ML_IN_KEYS_TOML));
static NE_NP_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ne-NP", NE_NP_KEYS_TOML));
static SI_LK_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("si-LK", SI_LK_KEYS_TOML));
static MY_MM_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("my-MM", MY_MM_KEYS_TOML));
static KM_KH_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("km-KH", KM_KH_KEYS_TOML));
static LO_LA_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("lo-LA", LO_LA_KEYS_TOML));
static MN_MN_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("mn-MN", MN_MN_KEYS_TOML));
static HA_NG_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ha-NG", HA_NG_KEYS_TOML));
static YO_NG_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("yo-NG", YO_NG_KEYS_TOML));
static IG_NG_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ig-NG", IG_NG_KEYS_TOML));
static AM_ET_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("am-ET", AM_ET_KEYS_TOML));
static OM_ET_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("om-ET", OM_ET_KEYS_TOML));
static SO_SO_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("so-SO", SO_SO_KEYS_TOML));
static ZU_ZA_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("zu-ZA", ZU_ZA_KEYS_TOML));
static AF_ZA_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("af-ZA", AF_ZA_KEYS_TOML));
static CA_ES_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ca-ES", CA_ES_KEYS_TOML));
static EU_ES_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("eu-ES", EU_ES_KEYS_TOML));
static GL_ES_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("gl-ES", GL_ES_KEYS_TOML));
static CY_GB_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("cy-GB", CY_GB_KEYS_TOML));
static SQ_AL_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("sq-AL", SQ_AL_KEYS_TOML));
static BS_BA_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("bs-BA", BS_BA_KEYS_TOML));
static MK_MK_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("mk-MK", MK_MK_KEYS_TOML));
static MT_MT_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("mt-MT", MT_MT_KEYS_TOML));
static HY_AM_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("hy-AM", HY_AM_KEYS_TOML));
static KA_GE_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ka-GE", KA_GE_KEYS_TOML));
static AZ_AZ_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("az-AZ", AZ_AZ_KEYS_TOML));
static KK_KZ_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("kk-KZ", KK_KZ_KEYS_TOML));
static UZ_UZ_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("uz-UZ", UZ_UZ_KEYS_TOML));
static KY_KG_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ky-KG", KY_KG_KEYS_TOML));
static TG_TJ_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("tg-TJ", TG_TJ_KEYS_TOML));
static TK_TM_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("tk-TM", TK_TM_KEYS_TOML));
static PS_AF_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ps-AF", PS_AF_KEYS_TOML));
static CKB_IQ_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ckb-IQ", CKB_IQ_KEYS_TOML));
static KU_TR_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ku-TR", KU_TR_KEYS_TOML));
static TI_ER_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("ti-ER", TI_ER_KEYS_TOML));
static RW_RW_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("rw-RW", RW_RW_KEYS_TOML));
static MG_MG_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("mg-MG", MG_MG_KEYS_TOML));
static SN_ZW_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("sn-ZW", SN_ZW_KEYS_TOML));
static XH_ZA_KEYS: LazyLock<toml::Value> =
    LazyLock::new(|| parse_locale_keys("xh-ZA", XH_ZA_KEYS_TOML));

fn validate_locale_config(config: &LocaleConfig) -> Result<(), String> {
    if config.default_locale != DEFAULT_LOCALE_ID {
        return Err(format!("default locale must be {DEFAULT_LOCALE_ID}"));
    }
    if config.locales.is_empty() {
        return Err("at least one locale is required".to_string());
    }

    let mut locale_ids = std::collections::BTreeSet::new();
    let mut prefixes = std::collections::BTreeSet::new();
    for locale in &config.locales {
        if locale.locale_id.trim().is_empty()
            || locale.html_lang.trim().is_empty()
            || locale.url_prefix.trim().is_empty()
            || locale.display_name.trim().is_empty()
            || locale.flag.trim().is_empty()
        {
            return Err("locale fields must not be empty".to_string());
        }
        if !locale_ids.insert(locale.locale_id.as_str()) {
            return Err(format!("duplicate locale id {}", locale.locale_id));
        }
        if !prefixes.insert(locale.url_prefix.as_str()) {
            return Err(format!("duplicate locale prefix {}", locale.url_prefix));
        }
        if locale.url_prefix != locale.url_prefix.to_ascii_lowercase()
            || locale.url_prefix.contains('/')
            || locale.url_prefix.contains(char::is_whitespace)
        {
            return Err(format!("invalid locale prefix {}", locale.url_prefix));
        }
        if let Some(dir) = locale.dir.as_deref()
            && dir != "ltr"
            && dir != "rtl"
        {
            return Err(format!("invalid locale direction {dir}"));
        }
    }
    if !locale_ids.contains(config.default_locale.as_str()) {
        return Err("default locale missing from locale list".to_string());
    }
    Ok(())
}

fn parse_locale_keys(expected_locale_id: &str, raw: &str) -> toml::Value {
    let keys = toml::from_str::<toml::Value>(raw)
        .unwrap_or_else(|error| panic!("valid i18n key TOML for {expected_locale_id}: {error}"));
    let locale_id = keys
        .get("locale_id")
        .and_then(toml::Value::as_str)
        .unwrap_or_default();
    assert_eq!(
        locale_id, expected_locale_id,
        "i18n key locale_id must match filename"
    );
    keys
}

fn locales() -> &'static [Locale] {
    &LOCALE_CONFIG.locales
}

fn default_locale() -> &'static Locale {
    locale_by_id(DEFAULT_LOCALE_ID).expect("validated default locale exists")
}

fn locale_by_id(locale_id: &str) -> Option<&'static Locale> {
    locales()
        .iter()
        .find(|locale| locale.locale_id == locale_id)
}

fn locale_by_prefix(prefix: &str) -> Option<&'static Locale> {
    let prefix = prefix.to_ascii_lowercase();
    locales().iter().find(|locale| locale.url_prefix == prefix)
}

fn locale_keys(locale_id: &str) -> &'static toml::Value {
    match locale_id {
        "en-GB" => &EN_GB_KEYS,
        "en-US" => &EN_US_KEYS,
        "fr-FR" => &FR_FR_KEYS,
        "de-DE" => &DE_DE_KEYS,
        "sv-SE" => &SV_SE_KEYS,
        "nb-NO" => &NB_NO_KEYS,
        "nl-NL" => &NL_NL_KEYS,
        "fi-FI" => &FI_FI_KEYS,
        "is-IS" => &IS_IS_KEYS,
        "es-ES" => &ES_ES_KEYS,
        "pt-PT" => &PT_PT_KEYS,
        "it-IT" => &IT_IT_KEYS,
        "ja-JP" => &JA_JP_KEYS,
        "zh-CN" => &ZH_CN_KEYS,
        "zh-TW" => &ZH_TW_KEYS,
        "vi-VN" => &VI_VN_KEYS,
        "th-TH" => &TH_TH_KEYS,
        "hi-IN" => &HI_IN_KEYS,
        "bn-IN" => &BN_IN_KEYS,
        "ta-IN" => &TA_IN_KEYS,
        "eo-001" => &EO_001_KEYS,
        "da-DK" => &DA_DK_KEYS,
        "la-VA" => &LA_VA_KEYS,
        "gsw-CH" => &GSW_CH_KEYS,
        "ko-KR" => &KO_KR_KEYS,
        "ru-RU" => &RU_RU_KEYS,
        "uk-UA" => &UK_UA_KEYS,
        "tr-TR" => &TR_TR_KEYS,
        "lt-LT" => &LT_LT_KEYS,
        "lv-LV" => &LV_LV_KEYS,
        "pl-PL" => &PL_PL_KEYS,
        "el-GR" => &EL_GR_KEYS,
        "hu-HU" => &HU_HU_KEYS,
        "et-EE" => &ET_EE_KEYS,
        "ovd-SE" => &OVD_SE_KEYS,
        "bg-BG" => &BG_BG_KEYS,
        "cs-CZ" => &CS_CZ_KEYS,
        "hr-HR" => &HR_HR_KEYS,
        "be-BY" => &BE_BY_KEYS,
        "ga-IE" => &GA_IE_KEYS,
        "lb-LU" => &LB_LU_KEYS,
        "ro-RO" => &RO_RO_KEYS,
        "sr-RS" => &SR_RS_KEYS,
        "nap-IT" => &NAP_IT_KEYS,
        "sk-SK" => &SK_SK_KEYS,
        "sl-SI" => &SL_SI_KEYS,
        "fy-NL" => &FY_NL_KEYS,
        "se-NO" => &SE_NO_KEYS,
        "scn-IT" => &SCN_IT_KEYS,
        "ar-001" | "ar-AE" | "ar-EG" | "ar-SA" => &AR_001_KEYS,
        "id-ID" => &ID_ID_KEYS,
        "ur-PK" => &UR_PK_KEYS,
        "mr-IN" => &MR_IN_KEYS,
        "jv-ID" => &JV_ID_KEYS,
        "pt-BR" => &PT_PT_KEYS,
        "es-MX" => &ES_ES_KEYS,
        "ms-MY" => &MS_MY_KEYS,
        "fil-PH" => &FIL_PH_KEYS,
        "fa-IR" => &FA_IR_KEYS,
        "he-IL" => &HE_IL_KEYS,
        "sw-KE" => &SW_KE_KEYS,
        "pa-IN" => &PA_IN_KEYS,
        "te-IN" => &TE_IN_KEYS,
        "gu-IN" => &GU_IN_KEYS,
        "kn-IN" => &KN_IN_KEYS,
        "ml-IN" => &ML_IN_KEYS,
        "ne-NP" => &NE_NP_KEYS,
        "si-LK" => &SI_LK_KEYS,
        "my-MM" => &MY_MM_KEYS,
        "km-KH" => &KM_KH_KEYS,
        "lo-LA" => &LO_LA_KEYS,
        "mn-MN" => &MN_MN_KEYS,
        "ha-NG" => &HA_NG_KEYS,
        "yo-NG" => &YO_NG_KEYS,
        "ig-NG" => &IG_NG_KEYS,
        "am-ET" => &AM_ET_KEYS,
        "om-ET" => &OM_ET_KEYS,
        "so-SO" => &SO_SO_KEYS,
        "zu-ZA" => &ZU_ZA_KEYS,
        "af-ZA" => &AF_ZA_KEYS,
        "ca-ES" => &CA_ES_KEYS,
        "eu-ES" => &EU_ES_KEYS,
        "gl-ES" => &GL_ES_KEYS,
        "cy-GB" => &CY_GB_KEYS,
        "sq-AL" => &SQ_AL_KEYS,
        "bs-BA" => &BS_BA_KEYS,
        "mk-MK" => &MK_MK_KEYS,
        "mt-MT" => &MT_MT_KEYS,
        "hy-AM" => &HY_AM_KEYS,
        "ka-GE" => &KA_GE_KEYS,
        "az-AZ" => &AZ_AZ_KEYS,
        "kk-KZ" => &KK_KZ_KEYS,
        "uz-UZ" => &UZ_UZ_KEYS,
        "ky-KG" => &KY_KG_KEYS,
        "tg-TJ" => &TG_TJ_KEYS,
        "tk-TM" => &TK_TM_KEYS,
        "ps-AF" => &PS_AF_KEYS,
        "ckb-IQ" => &CKB_IQ_KEYS,
        "ku-TR" => &KU_TR_KEYS,
        "ti-ER" => &TI_ER_KEYS,
        "rw-RW" => &RW_RW_KEYS,
        "mg-MG" => &MG_MG_KEYS,
        "sn-ZW" => &SN_ZW_KEYS,
        "xh-ZA" => &XH_ZA_KEYS,
        "nl-BE" => &NL_NL_KEYS,
        "fr-BE" | "fr-CA" => &FR_FR_KEYS,
        "en-CA" => &EN_GB_KEYS,
        _ => &EN_EU_KEYS,
    }
}

fn i18n(locale: &'static Locale) -> I18n {
    I18n {
        locale,
        keys: locale_keys(&locale.locale_id),
    }
}

impl I18n {
    fn t(&self, key: &str, fallback: &str) -> String {
        self.keys
            .get(key)
            .and_then(toml::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                key.split('.')
                    .try_fold(self.keys, |value, part| value.get(part))
                    .and_then(toml::Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| fallback.to_string())
            })
    }

    fn t_attr(&self, key: &str, fallback: &str) -> String {
        escape_html_attribute(&self.t(key, fallback))
    }

    fn locale_id(&self) -> &str {
        &self.locale.locale_id
    }

    fn html_lang(&self) -> &str {
        &self.locale.html_lang
    }

    fn dir(&self) -> &str {
        self.locale.dir.as_deref().unwrap_or("ltr")
    }
}

fn localized_path(locale: &Locale, path: &str) -> String {
    let path = path.trim_matches('/');
    if locale.locale_id == DEFAULT_LOCALE_ID {
        if path.is_empty() {
            "/".to_string()
        } else {
            format!("/{path}")
        }
    } else if path.is_empty() {
        format!("/{}/", locale.url_prefix)
    } else {
        format!("/{}/{path}", locale.url_prefix)
    }
}

fn split_locale_path(path: &str) -> (&'static Locale, String) {
    let clean = path.trim_matches('/');
    let mut parts = clean.splitn(2, '/');
    let first = parts.next().unwrap_or_default();
    if let Some(locale) = locale_by_prefix(first) {
        return (
            locale,
            parts
                .next()
                .unwrap_or_default()
                .trim_matches('/')
                .to_string(),
        );
    }
    (default_locale(), clean.to_string())
}

struct AppState {
    storage: Option<Arc<S3Storage>>,
    trusted_proxies: TrustedProxies,
    rate_limiter: RateLimiter,
    metrics: Metrics,
    observability: Observability,
}

impl Clone for AppState {
    fn clone(&self) -> Self {
        Self {
            storage: self.storage.clone(),
            trusted_proxies: self.trusted_proxies.clone(),
            rate_limiter: self.rate_limiter.clone(),
            metrics: self.metrics.clone(),
            observability: self.observability.clone(),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();

    let host = std::env::var("PUBLIC_WEBSITE_HOST").unwrap_or_else(|_| DEFAULT_HOST.to_string());
    let port = std::env::var("PORT")
        .ok()
        .and_then(|raw| raw.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let address: SocketAddr = format!("{host}:{port}").parse()?;
    let (observability, telemetry_guard) = Observability::from_env(env!("CARGO_PKG_VERSION"));

    let state = AppState {
        storage: S3Storage::from_env().await?.map(Arc::new),
        trusted_proxies: TrustedProxies::from_env()?,
        rate_limiter: RateLimiter::default(),
        metrics: Metrics::default(),
        observability,
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/help", get(help_page))
        .route("/docs", get(docs_page))
        .route("/docs/openapi.json", get(openapi_json))
        .route("/terms", get(terms_page))
        .route("/privacy", get(privacy_page))
        .route("/{locale}", get(localized_index))
        .route("/{locale}/", get(localized_index))
        .route("/{locale}/help", get(localized_help_page))
        .route("/{locale}/docs", get(localized_docs_page))
        .route("/{locale}/terms", get(localized_terms_page))
        .route("/{locale}/privacy", get(localized_privacy_page))
        .route("/robots.txt", get(robots_txt))
        .route("/sitemap.xml", get(sitemap_xml))
        .route("/favicon.svg", get(favicon_svg))
        .route("/site.webmanifest", get(site_webmanifest))
        .route("/og.png", get(og_png))
        .route(
            "/metrics",
            get(metrics_json).route_layer(middleware::from_fn(require_loopback_peer)),
        )
        .route("/healthz", get(healthz))
        .route("/telemetry/page-visible", post(telemetry_page_visible))
        .route("/telemetry/click", post(telemetry_click))
        .route(
            "/telemetry/avatar-generate",
            post(telemetry_avatar_generate),
        )
        .route("/v1/avatar", get(query_avatar))
        .route("/v1/avatar/link", get(query_avatar_link))
        .route("/avatar/{kind}/{identity}/{format}", get(path_avatar))
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state, observe_request))
        .layer(middleware::from_fn(add_security_headers));

    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(service = SITE_NAME, %address, "listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    telemetry_guard.shutdown();
    Ok(())
}

fn init_logging() {
    let _ = tracing_subscriber::fmt::try_init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "failed to listen for ctrl-c");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => tracing::warn!(%error, "failed to listen for sigterm"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

#[derive(Clone)]
struct Observability {
    enabled: bool,
    requests: Counter<u64>,
    request_duration: Histogram<f64>,
    page_views: Counter<u64>,
    page_visible: Histogram<f64>,
    outbound_clicks: Counter<u64>,
    ui_actions: Counter<u64>,
    ui_avatar_generations: Counter<u64>,
    avatar_renders: Counter<u64>,
    avatar_generation_duration: Histogram<f64>,
}

#[derive(Debug, Default)]
struct TelemetryGuard {
    meter_provider: Option<SdkMeterProvider>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RequestLabels {
    route: String,
    section: &'static str,
    country: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VisiblePageEvent {
    route: String,
    section: &'static str,
    seconds: u64,
}

impl Observability {
    fn disabled() -> Self {
        Self::from_meter(false)
    }

    fn from_env(service_version: &str) -> (Self, TelemetryGuard) {
        if !otlp_enabled_from_env() {
            return (Self::disabled(), TelemetryGuard::default());
        }

        match init_otel_metrics(service_version) {
            Ok(guard) => (Self::from_meter(true), guard),
            Err(error) => {
                tracing::warn!(%error, "OpenTelemetry disabled after initialization failure");
                (Self::disabled(), TelemetryGuard::default())
            }
        }
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn classify_request(path: &str) -> RequestLabels {
        RequestLabels {
            route: normalize_route(path),
            section: section_for_path(path),
            country: COUNTRY_UNKNOWN,
        }
    }

    fn validate_visible_event(event: VisiblePageEvent) -> Option<VisiblePageEvent> {
        if !is_allowed_visible_route(&event.route) || !is_allowed_visible_section(event.section) {
            return None;
        }
        Some(VisiblePageEvent {
            seconds: event.seconds.min(MAX_VISIBLE_SECONDS),
            ..event
        })
    }

    fn record_request(&self, labels: &RequestLabels, status: StatusCode, elapsed: Duration) {
        if !self.enabled {
            return;
        }
        let attributes = request_attributes(labels, status_class(status));
        self.requests.add(1, &attributes);
        self.request_duration
            .record(elapsed.as_secs_f64(), &attributes);
    }

    fn record_page_view(&self, labels: &RequestLabels) {
        if !self.enabled || !is_page_view_section(labels.section) {
            return;
        }
        self.page_views.add(
            1,
            &[
                KeyValue::new("route", labels.route.clone()),
                KeyValue::new("section", labels.section),
                KeyValue::new("country", labels.country),
            ],
        );
    }

    fn record_visible_page(&self, event: &VisiblePageEvent) {
        if !self.enabled {
            return;
        }
        self.page_visible.record(
            event.seconds as f64,
            &[
                KeyValue::new("route", event.route.clone()),
                KeyValue::new("section", event.section),
            ],
        );
    }

    fn record_click_for_locale(&self, locale: &str, kind: &str, target: &str) {
        if !self.enabled {
            return;
        }
        let attributes = [
            KeyValue::new("kind", kind.to_owned()),
            KeyValue::new("target", target.to_owned()),
            KeyValue::new("locale", locale.to_owned()),
            KeyValue::new("country", COUNTRY_UNKNOWN),
        ];
        if kind == "github" || kind == "outbound" {
            self.outbound_clicks.add(1, &attributes);
        } else {
            self.ui_actions.add(1, &attributes);
        }
    }

    fn record_ui_avatar_generation_for_locale(&self, locale: &str, style: &AvatarTelemetryStyle) {
        if !self.enabled {
            return;
        }
        let mut attributes = avatar_style_attributes("ui", style);
        attributes.push(KeyValue::new("locale", locale.to_owned()));
        self.ui_avatar_generations.add(1, &attributes);
    }

    fn record_avatar_render(
        &self,
        route: &'static str,
        request: &AvatarRequest,
        elapsed: Duration,
    ) {
        if !self.enabled {
            return;
        }
        let style = AvatarTelemetryStyle::from_request(request);
        let attributes = avatar_style_attributes(route, &style);
        self.avatar_renders.add(1, &attributes);
        self.avatar_generation_duration
            .record(elapsed.as_secs_f64(), &attributes);
    }

    fn from_meter(enabled: bool) -> Self {
        let meter = global::meter(OTEL_SERVICE_NAME);
        Self {
            enabled,
            requests: meter.u64_counter("hashavatar_api_requests_total").build(),
            request_duration: meter
                .f64_histogram("hashavatar_api_request_duration_seconds")
                .build(),
            page_views: meter.u64_counter("hashavatar_api_page_views_total").build(),
            page_visible: meter
                .f64_histogram("hashavatar_api_page_visible_seconds")
                .build(),
            outbound_clicks: meter
                .u64_counter("hashavatar_api_outbound_clicks_total")
                .build(),
            ui_actions: meter.u64_counter("hashavatar_api_ui_actions_total").build(),
            ui_avatar_generations: meter
                .u64_counter("hashavatar_api_ui_avatar_generations_total")
                .build(),
            avatar_renders: meter
                .u64_counter("hashavatar_api_avatar_renders_total")
                .build(),
            avatar_generation_duration: meter
                .f64_histogram("hashavatar_api_avatar_generation_duration_seconds")
                .build(),
        }
    }
}

impl TelemetryGuard {
    fn shutdown(self) {
        if let Some(provider) = self.meter_provider
            && let Err(error) = provider.shutdown()
        {
            tracing::warn!(?error, "failed to shutdown meter provider");
        }
    }
}

fn init_otel_metrics(
    _service_version: &str,
) -> Result<TelemetryGuard, Box<dyn std::error::Error + Send + Sync + 'static>> {
    let mut exporter_builder = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary);
    if let Some(endpoint) = otlp_endpoint_from_env() {
        exporter_builder = exporter_builder.with_endpoint(endpoint);
    }
    let metric_exporter = exporter_builder.build()?;
    let meter_provider = SdkMeterProvider::builder()
        .with_periodic_exporter(metric_exporter)
        .build();
    global::set_meter_provider(meter_provider.clone());
    Ok(TelemetryGuard {
        meter_provider: Some(meter_provider),
    })
}

fn otlp_enabled_from_env() -> bool {
    otlp_enabled_from_vars(|name| std::env::var(name).ok())
}

fn otlp_enabled_from_vars(mut value_for: impl FnMut(&str) -> Option<String>) -> bool {
    for name in [
        "HASHAVATAR_OTLP",
        "HASHAVATAR_OTLP_ENABLED",
        "HASHAVATAR_OTEL_ENABLED",
    ] {
        if let Some(value) = value_for(name).and_then(|value| parse_env_switch(&value)) {
            return value;
        }
    }
    false
}

fn otlp_endpoint_from_env() -> Option<String> {
    [
        "HASHAVATAR_OTLP_ENDPOINT",
        "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
        "OTEL_EXPORTER_OTLP_ENDPOINT",
    ]
    .into_iter()
    .find_map(|name| std::env::var(name).ok())
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())
}

fn parse_env_switch(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" | "enabled" | "enable" => Some(true),
        "0" | "false" | "no" | "off" | "disabled" | "disable" => Some(false),
        _ => None,
    }
}

fn normalize_route(path: &str) -> String {
    let raw_clean = path
        .split_once('?')
        .map_or(path, |(without_query, _query)| without_query)
        .trim_matches('/');
    let (_locale, localized_clean) = split_locale_path(raw_clean);
    let clean = localized_clean.trim_matches('/');
    match clean {
        "" => "/".to_string(),
        "help" => "/help".to_string(),
        "docs" => "/docs".to_string(),
        "docs/openapi.json" => "/docs/openapi.json".to_string(),
        "terms" => "/terms".to_string(),
        "privacy" => "/privacy".to_string(),
        "robots.txt" => "/robots.txt".to_string(),
        "sitemap.xml" => "/sitemap.xml".to_string(),
        "favicon.svg" => "/favicon.svg".to_string(),
        "site.webmanifest" => "/site.webmanifest".to_string(),
        "og.png" => "/og.png".to_string(),
        "healthz" => "/healthz".to_string(),
        "metrics" => "/metrics".to_string(),
        "v1/avatar" => "/v1/avatar".to_string(),
        "v1/avatar/link" => "/v1/avatar/link".to_string(),
        value if value.starts_with("avatar/") => "/avatar/:kind/:identity/:format".to_string(),
        value if value.starts_with("telemetry/") => "/telemetry/:event".to_string(),
        _ => "unknown".to_string(),
    }
}

fn section_for_path(path: &str) -> &'static str {
    let route = normalize_route(path);
    match route.as_str() {
        "/" => "home",
        "/help" | "/docs" | "/docs/openapi.json" => "docs",
        "/terms" | "/privacy" => "legal",
        "/v1/avatar" | "/v1/avatar/link" | "/avatar/:kind/:identity/:format" => "avatar",
        "/og.png" => "preview",
        "/robots.txt" | "/sitemap.xml" | "/favicon.svg" | "/site.webmanifest" => "static",
        "/healthz" | "/metrics" => "ops",
        "/telemetry/:event" => "telemetry",
        _ => "not_found",
    }
}

fn request_attributes(labels: &RequestLabels, status_class: &'static str) -> Vec<KeyValue> {
    vec![
        KeyValue::new("route", labels.route.clone()),
        KeyValue::new("section", labels.section),
        KeyValue::new("status_class", status_class),
        KeyValue::new("country", labels.country),
    ]
}

fn status_class(status: StatusCode) -> &'static str {
    match status.as_u16() {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "unknown",
    }
}

fn is_page_view_section(section: &str) -> bool {
    matches!(section, "home" | "docs" | "legal")
}

fn is_allowed_visible_route(route: &str) -> bool {
    matches!(route, "/" | "/help" | "/docs" | "/terms" | "/privacy")
}

fn is_allowed_visible_section(section: &str) -> bool {
    matches!(section, "home" | "docs" | "legal")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AvatarTelemetryStyle {
    kind: AvatarKind,
    background: AvatarBackground,
    accessory: AvatarAccessory,
    color: AvatarColor,
    expression: AvatarExpression,
    shape: AvatarShape,
    size_bucket: &'static str,
}

impl AvatarTelemetryStyle {
    fn from_request(request: &AvatarRequest) -> Self {
        Self {
            kind: request.kind,
            background: request.background,
            accessory: request.effective_accessory(),
            color: request.color,
            expression: request.effective_expression(),
            shape: request.shape,
            size_bucket: avatar_size_bucket(request.size),
        }
    }
}

fn avatar_style_attributes(route: &'static str, style: &AvatarTelemetryStyle) -> Vec<KeyValue> {
    vec![
        KeyValue::new("route", route),
        KeyValue::new("kind", style.kind.as_str()),
        KeyValue::new("background", style.background.as_str()),
        KeyValue::new("accessory", style.accessory.as_str()),
        KeyValue::new("color", style.color.as_str()),
        KeyValue::new("expression", style.expression.as_str()),
        KeyValue::new("shape", style.shape.as_str()),
        KeyValue::new("size_bucket", style.size_bucket),
    ]
}

fn avatar_size_bucket(size: u32) -> &'static str {
    match size {
        0..=127 => "64-127",
        128..=255 => "128-255",
        256..=511 => "256-511",
        _ => "512-1024",
    }
}

#[derive(Clone)]
struct CspNonce(String);

impl CspNonce {
    fn as_str(&self) -> &str {
        &self.0
    }
}

fn generate_csp_nonce() -> Result<CspNonce, getrandom::Error> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)?;

    let mut nonce = String::with_capacity(32);
    for byte in bytes {
        nonce.push_str(&format!("{byte:02x}"));
    }
    Ok(CspNonce(nonce))
}

fn content_security_policy(nonce: &CspNonce) -> String {
    format!(
        "default-src 'self'; base-uri 'self'; object-src 'none'; frame-ancestors 'none'; img-src 'self' data:; style-src 'self' 'nonce-{nonce}'; script-src 'self' 'nonce-{nonce}' {script_hash} {script_hash_compat}; connect-src 'self'; form-action 'self'",
        nonce = nonce.as_str(),
        script_hash = INDEX_SCRIPT_SHA256,
        script_hash_compat = INDEX_SCRIPT_SHA256_COMPAT,
    )
}

fn static_content_security_policy() -> &'static str {
    "default-src 'self'; base-uri 'self'; object-src 'none'; frame-ancestors 'none'; img-src 'self' data:; style-src 'self'; script-src 'self'; connect-src 'self'; form-action 'self'"
}

async fn add_security_headers(mut request: Request, next: Next) -> Response {
    let csp_nonce = if route_uses_inline_html(request.uri().path()) {
        let nonce = match generate_csp_nonce() {
            Ok(nonce) => nonce,
            Err(error) => return secure_rng_failure(error),
        };
        request.extensions_mut().insert(nonce.clone());
        Some(nonce)
    } else {
        None
    };

    let mut response = next.run(request).await;
    let is_html_response = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| content_type.starts_with("text/html"));
    let csp = csp_nonce
        .as_ref()
        .map(content_security_policy)
        .unwrap_or_else(|| static_content_security_policy().to_string());
    apply_security_headers(response.headers_mut(), &csp, is_html_response);

    response
}

fn route_uses_inline_html(path: &str) -> bool {
    let (_locale, slug) = split_locale_path(path);
    matches!(slug.as_str(), "" | "help" | "docs" | "terms" | "privacy")
}

fn apply_security_headers(headers: &mut HeaderMap, csp: &str, is_html_response: bool) {
    headers.insert(
        header::HeaderName::from_static("content-security-policy"),
        HeaderValue::from_str(csp).unwrap_or_else(|error| {
            tracing::error!(%error, "CSP header value rejected; falling back to static policy");
            HeaderValue::from_static(static_content_security_policy())
        }),
    );
    headers.insert(
        header::HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static(
            "camera=(), microphone=(), geolocation=(), payment=(), accelerometer=(), gyroscope=(), magnetometer=(), usb=(), serial=(), bluetooth=(), xr-spatial-tracking=(), clipboard-read=(), clipboard-write=(), screen-wake-lock=(), idle-detection=()",
        ),
    );
    headers.insert(
        header::HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    headers.insert(
        header::HeaderName::from_static("x-permitted-cross-domain-policies"),
        HeaderValue::from_static("none"),
    );
    headers.insert(
        header::HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("cross-origin"),
    );
    if is_html_response {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, max-age=0"),
        );
        headers.insert(
            header::HeaderName::from_static("cross-origin-opener-policy"),
            HeaderValue::from_static("same-origin"),
        );
    }
    headers.insert(
        header::HeaderName::from_static("strict-transport-security"),
        HeaderValue::from_static("max-age=31536000; includeSubDomains"),
    );
}

async fn require_loopback_peer(
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    if !is_loopback_peer(peer_addr) {
        return StatusCode::NOT_FOUND.into_response();
    }
    next.run(request).await
}

fn is_loopback_peer(peer_addr: SocketAddr) -> bool {
    normalize_ip(peer_addr.ip()).is_loopback()
}

fn secure_rng_failure(error: getrandom::Error) -> Response {
    tracing::error!(%error, "secure RNG failure; refusing to generate CSP nonce");
    let mut response = (StatusCode::SERVICE_UNAVAILABLE, INTERNAL_ERROR_MESSAGE).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static("default-src 'none'; base-uri 'none'; frame-ancestors 'none'"),
    );
    headers.insert(
        header::HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static(
            "camera=(), microphone=(), geolocation=(), payment=(), accelerometer=(), gyroscope=(), magnetometer=(), usb=(), serial=(), bluetooth=(), xr-spatial-tracking=(), clipboard-read=(), clipboard-write=(), screen-wake-lock=(), idle-detection=()",
        ),
    );
    headers.insert(
        header::HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    headers.insert(
        header::HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("cross-origin"),
    );
    headers.insert(
        header::HeaderName::from_static("x-permitted-cross-domain-policies"),
        HeaderValue::from_static("none"),
    );
    apply_no_store(headers);
    response
}

async fn observe_request(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let method_is_get = request.method() == axum::http::Method::GET;
    let labels = Observability::classify_request(request.uri().path());
    let started = Instant::now();
    let response = next.run(request).await;
    let status = response.status();

    state
        .observability
        .record_request(&labels, status, started.elapsed());
    if method_is_get && status.is_success() {
        state.observability.record_page_view(&labels);
    }

    response
}

async fn index(
    State(state): State<AppState>,
    Extension(csp_nonce): Extension<CspNonce>,
) -> Html<String> {
    Html(render_index_html(
        &csp_nonce,
        state.storage.is_some(),
        state.observability.enabled(),
        i18n(default_locale()),
    ))
}

async fn help_page(
    State(state): State<AppState>,
    Extension(csp_nonce): Extension<CspNonce>,
) -> Html<String> {
    Html(render_help_html(
        &csp_nonce,
        state.observability.enabled(),
        i18n(default_locale()),
    ))
}

async fn docs_page(
    State(state): State<AppState>,
    Extension(csp_nonce): Extension<CspNonce>,
) -> Html<String> {
    Html(render_docs_html(
        &csp_nonce,
        state.observability.enabled(),
        i18n(default_locale()),
    ))
}

async fn terms_page(
    State(state): State<AppState>,
    Extension(csp_nonce): Extension<CspNonce>,
) -> Html<String> {
    Html(render_terms_html(
        &csp_nonce,
        state.observability.enabled(),
        i18n(default_locale()),
    ))
}

async fn privacy_page(
    State(state): State<AppState>,
    Extension(csp_nonce): Extension<CspNonce>,
) -> Html<String> {
    Html(render_privacy_html(
        &csp_nonce,
        state.observability.enabled(),
        i18n(default_locale()),
    ))
}

async fn localized_index(
    State(state): State<AppState>,
    Extension(csp_nonce): Extension<CspNonce>,
    Path(locale): Path<String>,
) -> Response {
    let Some(locale) = locale_by_prefix(&locale) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Html(render_index_html(
        &csp_nonce,
        state.storage.is_some(),
        state.observability.enabled(),
        i18n(locale),
    ))
    .into_response()
}

async fn localized_help_page(
    State(state): State<AppState>,
    Extension(csp_nonce): Extension<CspNonce>,
    Path(locale): Path<String>,
) -> Response {
    let Some(locale) = locale_by_prefix(&locale) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Html(render_help_html(
        &csp_nonce,
        state.observability.enabled(),
        i18n(locale),
    ))
    .into_response()
}

async fn localized_docs_page(
    State(state): State<AppState>,
    Extension(csp_nonce): Extension<CspNonce>,
    Path(locale): Path<String>,
) -> Response {
    let Some(locale) = locale_by_prefix(&locale) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Html(render_docs_html(
        &csp_nonce,
        state.observability.enabled(),
        i18n(locale),
    ))
    .into_response()
}

async fn localized_terms_page(
    State(state): State<AppState>,
    Extension(csp_nonce): Extension<CspNonce>,
    Path(locale): Path<String>,
) -> Response {
    let Some(locale) = locale_by_prefix(&locale) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Html(render_terms_html(
        &csp_nonce,
        state.observability.enabled(),
        i18n(locale),
    ))
    .into_response()
}

async fn localized_privacy_page(
    State(state): State<AppState>,
    Extension(csp_nonce): Extension<CspNonce>,
    Path(locale): Path<String>,
) -> Response {
    let Some(locale) = locale_by_prefix(&locale) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Html(render_privacy_html(
        &csp_nonce,
        state.observability.enabled(),
        i18n(locale),
    ))
    .into_response()
}

async fn robots_txt() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        format!(
            "User-agent: *\nAllow: /\n\nSitemap: {}/sitemap.xml\n",
            SITE_URL
        ),
    )
}

async fn sitemap_xml() -> impl IntoResponse {
    let urls = ["", "help", "terms", "privacy"]
        .into_iter()
        .flat_map(|slug| {
            locales().iter().map(move |locale| {
                format!(
                    "  <url><loc>{site}{path}</loc></url>",
                    site = SITE_URL,
                    path = localized_path(locale, slug)
                )
            })
        })
        .collect::<Vec<_>>()
        .join("\n");
    (
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
{urls}
</urlset>"#
        ),
    )
}

async fn favicon_svg() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/svg+xml; charset=utf-8")],
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" rx="16" fill="#f7f0e6"/><ellipse cx="32" cy="34" rx="18" ry="16" fill="#8d4dcb"/><polygon points="20,25 24,10 30,24" fill="#4c2d68"/><polygon points="44,25 40,10 34,24" fill="#4c2d68"/><ellipse cx="25" cy="31" rx="4" ry="5" fill="#fcf8ec"/><ellipse cx="39" cy="31" rx="4" ry="5" fill="#fcf8ec"/><ellipse cx="25" cy="31" rx="2" ry="3" fill="#18141c"/><ellipse cx="39" cy="31" rx="2" ry="3" fill="#18141c"/><rect x="22" y="40" width="20" height="5" rx="2" fill="#301218"/></svg>"##.to_string(),
    )
}

async fn site_webmanifest() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/manifest+json; charset=utf-8",
        )],
        Json(serde_json::json!({
            "name": SITE_NAME,
            "short_name": "hashavatar",
            "start_url": "/",
            "display": "standalone",
            "background_color": "#fbf6ee",
            "theme_color": "#d97a42",
            "icons": [{
                "src": "/favicon.svg",
                "sizes": "64x64",
                "type": "image/svg+xml",
                "purpose": "any"
            }]
        })),
    )
}

async fn metrics_json(State(state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(state.metrics.snapshot(state.storage.is_some())),
    )
}

async fn openapi_json() -> impl IntoResponse {
    Json(openapi_document())
}

fn openapi_document() -> serde_json::Value {
    serde_json::json!({
        "openapi": "3.1.0",
        "info": {
            "title": "hashavatar.app API",
            "version": AVATAR_STYLE_VERSION.to_string(),
            "description": "Public procedural avatar API"
        },
        "servers": [{ "url": SITE_URL }],
        "paths": {
            "/v1/avatar": {
                "get": {
                    "summary": "Generate an avatar",
                    "parameters": [
                        {"name":"id","in":"query","schema":{"type":"string"}},
                        {"name":"tenant","in":"query","schema":{"type":"string"}},
                        {"name":"style_version","in":"query","schema":{"type":"string"}},
                        {"name":"algorithm","in":"query","schema":{"type":"string","enum": ["sha512"]}},
                        {"name":"kind","in":"query","schema":{"type":"string","enum": AvatarKind::ALL.iter().map(|kind| kind.as_str()).collect::<Vec<_>>()}},
                        {"name":"background","in":"query","schema":{"type":"string","enum": AvatarBackground::ALL.iter().map(|background| background.as_str()).collect::<Vec<_>>()}},
                        {"name":"accessory","in":"query","schema":{"type":"string","enum": AvatarAccessory::ALL.iter().map(|accessory| accessory.as_str()).collect::<Vec<_>>()}},
                        {"name":"color","in":"query","schema":{"type":"string","enum": AvatarColor::ALL.iter().map(|color| color.as_str()).collect::<Vec<_>>()}},
                        {"name":"expression","in":"query","schema":{"type":"string","enum": AvatarExpression::ALL.iter().map(|expression| expression.as_str()).collect::<Vec<_>>()}},
                        {"name":"shape","in":"query","schema":{"type":"string","enum": AvatarShape::ALL.iter().map(|shape| shape.as_str()).collect::<Vec<_>>()}},
                        {"name":"format","in":"query","schema":{"type":"string","enum":["webp"]}},
                        {"name":"size","in":"query","schema":{"type":"integer","minimum": MIN_SIZE, "maximum": MAX_SIZE}}
                    ],
                    "responses": {"200":{"description":"Avatar asset"}}
                }
            },
            "/v1/avatar/link": {
                "get": {
                    "summary": "Persist to object storage and return a signed link",
                    "responses": {"200":{"description":"Signed link metadata"}}
                }
            },
            "/avatar/{kind}/{identity}/{format}": {
                "get": {
                    "summary": "Path-style avatar URL",
                    "responses": {"200":{"description":"Avatar asset"}}
                }
            },
        }
    })
}

async fn og_png(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<OgQuery>,
) -> Response {
    if let Err(response) =
        enforce_limits(&state, &headers, peer_addr.ip(), RateLimitRoute::OgImage).await
    {
        return response;
    }

    let title_id = query
        .id
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|| DEFAULT_ID.to_string());
    if let Err(message) = validate_identity(&title_id) {
        return bad_request(&message);
    }

    let tenant = query
        .tenant
        .as_deref()
        .unwrap_or(DEFAULT_NAMESPACE_TENANT)
        .to_string();
    let style_version = query
        .style_version
        .as_deref()
        .unwrap_or(DEFAULT_NAMESPACE_STYLE)
        .to_string();
    if validate_namespace_component("tenant", &tenant).is_err()
        || validate_namespace_component("style_version", &style_version).is_err()
    {
        return bad_request(INVALID_NAMESPACE_MESSAGE);
    }

    let lead_kind = query
        .kind
        .as_deref()
        .and_then(|raw| AvatarKind::from_str(raw).ok())
        .unwrap_or(AvatarKind::Monster);

    let render_permit = match RENDER_SLOTS.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return server_busy(),
    };
    let render = tokio::task::spawn_blocking(move || {
        let _render_permit = render_permit;
        build_og_png_bytes(&title_id, &tenant, &style_version, lead_kind)
    });
    let bytes =
        match tokio::time::timeout(Duration::from_millis(AVATAR_TIMEOUT_MS * 3), render).await {
            Ok(Ok(Ok(bytes))) => bytes,
            Ok(Ok(Err(error))) => return error.into_response(),
            Ok(Err(error)) => return internal_error(error),
            Err(_) => return request_timeout("Open Graph image generation timed out"),
        };

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        bytes,
    )
        .into_response()
}

enum OgPngError {
    BadRequest(&'static str),
    Internal(String),
}

impl OgPngError {
    fn into_response(self) -> Response {
        match self {
            Self::BadRequest(message) => bad_request(message),
            Self::Internal(message) => internal_error(message),
        }
    }
}

fn build_og_png_bytes(
    title_id: &str,
    tenant: &str,
    style_version: &str,
    lead_kind: AvatarKind,
) -> Result<Vec<u8>, OgPngError> {
    let namespace = AvatarNamespace::new(tenant, style_version)
        .map_err(|_| OgPngError::BadRequest(INVALID_NAMESPACE_MESSAGE))?;
    let spec = AvatarSpec::new(220, 220, 0)
        .map_err(|_| OgPngError::BadRequest(INVALID_AVATAR_RENDER_MESSAGE))?;

    let mut canvas: RgbaImage = ImageBuffer::from_pixel(1200, 630, Rgba([251, 246, 238, 255]));
    draw_rect(&mut canvas, 0, 0, 1200, 630, Rgba([242, 236, 228, 255]));
    draw_circle(&mut canvas, 160, 140, 180, Rgba([255, 214, 170, 180]));
    draw_circle(&mut canvas, 1030, 500, 150, Rgba([217, 122, 66, 70]));

    for (idx, kind) in [lead_kind, AvatarKind::Robot, AvatarKind::Ghost]
        .into_iter()
        .enumerate()
    {
        let avatar = render_avatar_for_namespace(
            spec,
            namespace,
            title_id,
            AvatarOptions::new(
                kind,
                if idx == 1 {
                    AvatarBackground::White
                } else {
                    AvatarBackground::Themed
                },
            ),
        )
        .map_err(|_| OgPngError::BadRequest(INVALID_AVATAR_RENDER_MESSAGE))?;
        overlay(&mut canvas, &avatar, 110 + idx as u32 * 260, 180)
            .map_err(|error| OgPngError::Internal(error.to_string()))?;
    }

    use image::ImageEncoder;
    let mut buf = Vec::new();
    image::codecs::png::PngEncoder::new(&mut buf)
        .write_image(
            canvas.as_raw(),
            canvas.width(),
            canvas.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|error| OgPngError::Internal(error.to_string()))?;
    Ok(buf)
}

async fn healthz() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "ok",
        })),
    )
}

#[derive(Debug, Deserialize)]
struct VisibleTelemetryPayload {
    route: String,
    section: String,
    locale: String,
    seconds: u64,
}

#[derive(Debug, Deserialize)]
struct ClickTelemetryPayload {
    kind: String,
    target: String,
    locale: String,
}

#[derive(Debug, Deserialize)]
struct AvatarGenerateTelemetryPayload {
    locale: String,
    kind: String,
    background: String,
    accessory: String,
    color: String,
    expression: String,
    shape: String,
    size: u32,
}

async fn telemetry_page_visible(
    State(state): State<AppState>,
    Json(payload): Json<VisibleTelemetryPayload>,
) -> Response {
    if !state.observability.enabled() {
        return StatusCode::ACCEPTED.into_response();
    }

    let section = match visible_section_from_str(&payload.section) {
        Some(section) => section,
        None => return bad_request("invalid telemetry event"),
    };
    if !is_allowed_locale_id(&payload.locale) {
        return bad_request("invalid telemetry event");
    }
    let event = VisiblePageEvent {
        route: normalize_route(&payload.route),
        section,
        seconds: payload.seconds,
    };
    match Observability::validate_visible_event(event) {
        Some(event) => {
            state.observability.record_visible_page(&event);
            StatusCode::ACCEPTED.into_response()
        }
        None => bad_request("invalid telemetry event"),
    }
}

async fn telemetry_click(
    State(state): State<AppState>,
    Json(payload): Json<ClickTelemetryPayload>,
) -> Response {
    if !is_allowed_click(&payload.kind, &payload.target) || !is_allowed_locale_id(&payload.locale) {
        return bad_request("invalid telemetry event");
    }
    state
        .observability
        .record_click_for_locale(&payload.locale, &payload.kind, &payload.target);
    StatusCode::ACCEPTED.into_response()
}

async fn telemetry_avatar_generate(
    State(state): State<AppState>,
    Json(payload): Json<AvatarGenerateTelemetryPayload>,
) -> Response {
    let style = match avatar_telemetry_style_from_payload(&payload) {
        Ok(style) => style,
        Err(message) => return bad_request(message),
    };
    state
        .observability
        .record_ui_avatar_generation_for_locale(&payload.locale, &style);
    StatusCode::ACCEPTED.into_response()
}

fn is_allowed_locale_id(locale_id: &str) -> bool {
    locale_by_id(locale_id).is_some()
}

fn visible_section_from_str(section: &str) -> Option<&'static str> {
    match section {
        "home" => Some("home"),
        "docs" => Some("docs"),
        "legal" => Some("legal"),
        _ => None,
    }
}

fn is_allowed_click(kind: &str, target: &str) -> bool {
    matches!(
        (kind, target),
        ("github", "repository")
            | ("outbound", "crate")
            | ("action", "copy-url")
            | ("action", "copy-signed-link")
            | ("action", "download")
            | ("action", "open-raw")
            | ("action", "preset-card")
            | ("action", "preset-prev")
            | ("action", "preset-next")
    )
}

fn avatar_telemetry_style_from_payload(
    payload: &AvatarGenerateTelemetryPayload,
) -> Result<AvatarTelemetryStyle, &'static str> {
    if !(MIN_SIZE..=MAX_SIZE).contains(&payload.size) {
        return Err("invalid telemetry event");
    }
    if !is_allowed_locale_id(&payload.locale) {
        return Err("invalid telemetry event");
    }

    let kind = AvatarKind::from_str(&payload.kind).map_err(|_| "invalid telemetry event")?;
    let background =
        AvatarBackground::from_str(&payload.background).map_err(|_| "invalid telemetry event")?;
    let mut accessory =
        AvatarAccessory::from_str(&payload.accessory).map_err(|_| "invalid telemetry event")?;
    let color = AvatarColor::from_str(&payload.color).map_err(|_| "invalid telemetry event")?;
    let mut expression =
        AvatarExpression::from_str(&payload.expression).map_err(|_| "invalid telemetry event")?;
    let shape = AvatarShape::from_str(&payload.shape).map_err(|_| "invalid telemetry event")?;

    if !kind.supports_face_layers() {
        accessory = DEFAULT_ACCESSORY;
        expression = DEFAULT_EXPRESSION;
    }

    Ok(AvatarTelemetryStyle {
        kind,
        background,
        accessory,
        color,
        expression,
        shape,
        size_bucket: avatar_size_bucket(payload.size),
    })
}

#[derive(Clone, Copy)]
enum RateLimitRoute {
    Avatar,
    StorageLink,
    OgImage,
}

impl RateLimitRoute {
    fn as_str(self) -> &'static str {
        match self {
            Self::Avatar => "avatar",
            Self::StorageLink => "storage-link",
            Self::OgImage => "og-image",
        }
    }

    fn limit(self) -> u32 {
        match self {
            Self::Avatar => 240,
            Self::StorageLink => 30,
            Self::OgImage => 60,
        }
    }
}

#[derive(Clone)]
struct RateLimiter {
    shards: Arc<Vec<Mutex<RateLimiterState>>>,
}

#[derive(Clone, Copy)]
struct RateBucket {
    started_at: Instant,
    count: u32,
}

struct RateLimiterState {
    buckets: LruCache<String, RateBucket>,
}

impl RateLimiterState {
    fn new(capacity: usize) -> Self {
        let capacity =
            NonZeroUsize::new(capacity.max(1)).expect("rate limiter capacity is nonzero");
        Self {
            buckets: LruCache::new(capacity),
        }
    }

    fn bucket_for(&mut self, key: String, now: Instant) -> &mut RateBucket {
        if self.buckets.get(&key).is_none() {
            self.buckets.push(
                key.clone(),
                RateBucket {
                    started_at: now,
                    count: 0,
                },
            );
        }

        self.buckets
            .get_mut(&key)
            .expect("rate limiter bucket is present after insertion")
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.buckets.len()
    }
}

#[derive(Clone, Default)]
struct TrustedProxies {
    networks: Arc<Vec<IpNet>>,
}

impl TrustedProxies {
    fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        match std::env::var(TRUSTED_PROXIES_ENV) {
            Ok(raw) => Self::parse(&raw)
                .map_err(|message| format!("{TRUSTED_PROXIES_ENV}: {message}").into()),
            Err(std::env::VarError::NotPresent) => Ok(Self::default()),
            Err(error) => Err(Box::new(error)),
        }
    }

    fn parse(raw: &str) -> Result<Self, String> {
        let mut networks = Vec::new();
        for value in raw.split([',', ' ', '\n', '\t']) {
            let value = value.trim();
            if value.is_empty() {
                continue;
            }

            let network = value
                .parse::<IpNet>()
                .or_else(|_| value.parse::<IpAddr>().map(IpNet::from))
                .map_err(|_| format!("invalid trusted proxy address or CIDR: {value}"))?;
            networks.push(network);
        }

        Ok(Self {
            networks: Arc::new(networks),
        })
    }

    fn contains(&self, ip: IpAddr) -> bool {
        let ip = normalize_ip(ip);
        self.networks.iter().any(|network| network.contains(&ip))
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::with_capacity(MAX_RATE_LIMIT_BUCKETS)
    }
}

impl RateLimiter {
    fn with_capacity(capacity: usize) -> Self {
        let shard_count = RATE_LIMIT_SHARDS.min(capacity.max(1));
        let per_shard_capacity = capacity.max(1).div_ceil(shard_count);
        let shards = (0..shard_count)
            .map(|_| Mutex::new(RateLimiterState::new(per_shard_capacity)))
            .collect();

        Self {
            shards: Arc::new(shards),
        }
    }

    async fn check(&self, key: String, limit: u32) -> Result<(), u64> {
        let now = Instant::now();
        let shard_index = self.shard_index_for(&key);
        let mut buckets = self.shards[shard_index].lock().await;
        let bucket = buckets.bucket_for(key, now);
        if now.duration_since(bucket.started_at) >= RATE_LIMIT_WINDOW {
            bucket.started_at = now;
            bucket.count = 0;
        }
        if bucket.count >= limit {
            let elapsed = now.duration_since(bucket.started_at);
            let remaining = RATE_LIMIT_WINDOW.saturating_sub(elapsed).as_secs().max(1);
            return Err(remaining);
        }
        bucket.count += 1;
        Ok(())
    }

    fn shard_index_for(&self, key: &str) -> usize {
        stable_shard_hash(key) as usize % self.shards.len()
    }

    #[cfg(test)]
    async fn len(&self) -> usize {
        let mut len = 0;
        for shard in self.shards.iter() {
            len += shard.lock().await.len();
        }
        len
    }
}

#[derive(Default, Clone)]
struct Metrics {
    requests_total: Arc<AtomicU64>,
    avatar_rendered_total: Arc<AtomicU64>,
    avatar_link_total: Arc<AtomicU64>,
    limited_total: Arc<AtomicU64>,
    storage_write_total: Arc<AtomicU64>,
    storage_hit_total: Arc<AtomicU64>,
    storage_redirect_total: Arc<AtomicU64>,
    generation_millis_total: Arc<AtomicU64>,
    format_webp_total: Arc<AtomicU64>,
    format_png_total: Arc<AtomicU64>,
    format_jpeg_total: Arc<AtomicU64>,
    format_gif_total: Arc<AtomicU64>,
    format_svg_total: Arc<AtomicU64>,
}

fn stable_shard_hash(key: &str) -> u64 {
    let digest = Sha256::digest(key.as_bytes());
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes)
}

#[derive(Serialize)]
struct MetricsSnapshot {
    requests_total: u64,
    avatar_rendered_total: u64,
    avatar_link_total: u64,
    limited_total: u64,
    storage_write_total: u64,
    storage_hit_total: u64,
    storage_redirect_total: u64,
    generation_millis_total: u64,
    formats: serde_json::Value,
    s3_enabled: bool,
}

impl Metrics {
    fn increment_format(&self, format: &str) {
        match format {
            "webp" => {
                self.format_webp_total.fetch_add(1, Ordering::Relaxed);
            }
            "png" => {
                self.format_png_total.fetch_add(1, Ordering::Relaxed);
            }
            "jpg" => {
                self.format_jpeg_total.fetch_add(1, Ordering::Relaxed);
            }
            "gif" => {
                self.format_gif_total.fetch_add(1, Ordering::Relaxed);
            }
            "svg" => {
                self.format_svg_total.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    fn observe_generation(&self, duration: Duration) {
        let millis = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        self.generation_millis_total
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(millis))
            })
            .ok();
    }

    fn snapshot(&self, s3_enabled: bool) -> MetricsSnapshot {
        MetricsSnapshot {
            requests_total: self.requests_total.load(Ordering::Relaxed),
            avatar_rendered_total: self.avatar_rendered_total.load(Ordering::Relaxed),
            avatar_link_total: self.avatar_link_total.load(Ordering::Relaxed),
            limited_total: self.limited_total.load(Ordering::Relaxed),
            storage_write_total: self.storage_write_total.load(Ordering::Relaxed),
            storage_hit_total: self.storage_hit_total.load(Ordering::Relaxed),
            storage_redirect_total: self.storage_redirect_total.load(Ordering::Relaxed),
            generation_millis_total: self.generation_millis_total.load(Ordering::Relaxed),
            formats: serde_json::json!({
                "webp": self.format_webp_total.load(Ordering::Relaxed),
                "png": self.format_png_total.load(Ordering::Relaxed),
                "jpg": self.format_jpeg_total.load(Ordering::Relaxed),
                "gif": self.format_gif_total.load(Ordering::Relaxed),
                "svg": self.format_svg_total.load(Ordering::Relaxed),
            }),
            s3_enabled,
        }
    }
}

async fn enforce_limits(
    state: &AppState,
    headers: &HeaderMap,
    peer_ip: IpAddr,
    route: RateLimitRoute,
) -> Result<(), Response> {
    let ip = client_ip(headers, peer_ip, &state.trusted_proxies);
    let key = rate_limit_key(route, &ip);
    match state.rate_limiter.check(key, route.limit()).await {
        Ok(()) => Ok(()),
        Err(retry_after_secs) => {
            state.metrics.limited_total.fetch_add(1, Ordering::Relaxed);
            let mut response = (
                StatusCode::TOO_MANY_REQUESTS,
                "rate limit exceeded, please retry shortly".to_string(),
            )
                .into_response();
            response.headers_mut().insert(
                header::RETRY_AFTER,
                HeaderValue::from_str(&retry_after_secs.to_string())
                    .unwrap_or_else(|_| HeaderValue::from_static("60")),
            );
            response.headers_mut().insert(
                header::HeaderName::from_static("x-ratelimit-limit"),
                HeaderValue::from_str(&route.limit().to_string())
                    .unwrap_or_else(|_| HeaderValue::from_static("0")),
            );
            response.headers_mut().insert(
                header::HeaderName::from_static("x-ratelimit-remaining"),
                HeaderValue::from_static("0"),
            );
            response.headers_mut().insert(
                header::HeaderName::from_static("x-ratelimit-reset"),
                HeaderValue::from_str(&retry_after_secs.to_string())
                    .unwrap_or_else(|_| HeaderValue::from_static("60")),
            );
            apply_no_store(response.headers_mut());
            Err(response)
        }
    }
}

fn rate_limit_key(route: RateLimitRoute, ip: &str) -> String {
    format!("{}:{ip}", route.as_str())
}

fn client_ip(headers: &HeaderMap, peer_ip: IpAddr, trusted_proxies: &TrustedProxies) -> String {
    let peer_ip = normalize_ip(peer_ip);
    if !trusted_proxies.contains(peer_ip) {
        return peer_ip.to_string();
    }

    if let Some(ip) = single_ip_header(headers, "cf-connecting-ip")
        && is_global_client_ip(ip)
    {
        return ip.to_string();
    }

    if let Some(ip) = single_ip_header(headers, "x-real-ip")
        && is_global_client_ip(ip)
    {
        return ip.to_string();
    }

    if let Some(value) = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
    {
        for candidate in value.split(',').rev() {
            if let Ok(ip) = candidate.trim().parse::<IpAddr>().map(normalize_ip)
                && !trusted_proxies.contains(ip)
                && is_global_client_ip(ip)
            {
                return ip.to_string();
            }
        }
    }
    peer_ip.to_string()
}

fn is_global_client_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, d] = ip.octets();
            if ip.is_unspecified()
                || ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_multicast()
            {
                return false;
            }

            if a == 0
                || a >= 240
                || (a == 100 && (64..=127).contains(&b))
                || (a == 198 && (18..=19).contains(&b))
                || (a == 192 && b == 0 && c == 0 && d == 0)
            {
                return false;
            }

            true
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
                return false;
            }

            if (segments[0] & 0xfe00) == 0xfc00
                || (segments[0] & 0xffc0) == 0xfe80
                || (segments[0] == 0x2001 && segments[1] == 0x0db8)
            {
                return false;
            }

            true
        }
    }
}

fn single_ip_header(headers: &HeaderMap, header_name: &'static str) -> Option<IpAddr> {
    headers
        .get(header_name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<IpAddr>().ok())
        .map(normalize_ip)
}

fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ipv6) => ipv6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(ipv6)),
        IpAddr::V4(_) => ip,
    }
}

async fn query_avatar(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<AvatarQuery>,
) -> Response {
    let request = match AvatarRequest::from_query(query) {
        Ok(request) => request,
        Err(message) => return bad_request(&message),
    };

    let route = if request.persist {
        RateLimitRoute::StorageLink
    } else {
        RateLimitRoute::Avatar
    };
    if let Err(response) = enforce_limits(&state, &headers, peer_addr.ip(), route).await {
        return response;
    }
    serve_avatar(state, request).await
}

async fn query_avatar_link(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<AvatarQuery>,
) -> Response {
    let request = match AvatarRequest::from_query(query) {
        Ok(request) => request,
        Err(message) => return bad_request(&message),
    };

    if let Err(response) = enforce_limits(
        &state,
        &headers,
        peer_addr.ip(),
        RateLimitRoute::StorageLink,
    )
    .await
    {
        return response;
    }
    serve_avatar_link(state, request).await
}

async fn path_avatar(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(path): Path<PathAvatar>,
) -> Response {
    let kind = match AvatarKind::from_str(&path.kind) {
        Ok(kind) => kind,
        Err(_) => return bad_request("unsupported avatar kind"),
    };
    let format = match AvatarRequestFormat::from_str(&path.format) {
        Ok(format) => format,
        Err(_) => return bad_request(INVALID_AVATAR_FORMAT_MESSAGE),
    };

    let request = AvatarRequest {
        identity: path.identity,
        namespace_tenant: DEFAULT_NAMESPACE_TENANT.to_string(),
        namespace_style: DEFAULT_NAMESPACE_STYLE.to_string(),
        kind,
        background: AvatarBackground::Themed,
        accessory: DEFAULT_ACCESSORY,
        color: DEFAULT_COLOR,
        expression: DEFAULT_EXPRESSION,
        shape: DEFAULT_SHAPE,
        format,
        size: 256,
        persist: false,
        redirect: false,
    };
    if let Err(message) = request.validate() {
        return bad_request(&message);
    }

    if let Err(response) =
        enforce_limits(&state, &headers, peer_addr.ip(), RateLimitRoute::Avatar).await
    {
        return response;
    }
    serve_avatar(state, request).await
}

async fn serve_avatar(state: AppState, request: AvatarRequest) -> Response {
    state.metrics.requests_total.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    let asset = match generate_avatar_asset(request.clone()).await {
        Ok(asset) => asset,
        Err(response) => return response,
    };

    state
        .metrics
        .avatar_rendered_total
        .fetch_add(1, Ordering::Relaxed);
    let elapsed = started.elapsed();
    state.metrics.observe_generation(elapsed);
    state
        .observability
        .record_avatar_render("avatar", &request, elapsed);

    let format_name = request.format.as_str();
    state.metrics.increment_format(format_name);

    let etag = etag_for(&asset.cache_key);
    let mut headers = cache_headers(&etag);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(asset.content_type),
    );

    if request.persist {
        let storage = match state.storage.as_ref() {
            Some(storage) => storage,
            None => return bad_request("S3 storage is not configured on this server"),
        };

        match tokio::time::timeout(
            Duration::from_millis(STORAGE_TIMEOUT_MS),
            storage.store_and_sign(&asset, &state.metrics),
        )
        .await
        {
            Ok(Ok(signed)) => {
                if request.redirect {
                    state
                        .metrics
                        .storage_redirect_total
                        .fetch_add(1, Ordering::Relaxed);
                    return Redirect::temporary(&signed.signed_url).into_response();
                }
            }
            Ok(Err(error)) => return internal_error(error),
            Err(_) => return request_timeout("object storage timed out"),
        }
    }

    (StatusCode::OK, headers, asset.body).into_response()
}

async fn serve_avatar_link(state: AppState, request: AvatarRequest) -> Response {
    state.metrics.requests_total.fetch_add(1, Ordering::Relaxed);
    let storage = match state.storage.as_ref() {
        Some(storage) => storage,
        None => return bad_request("S3 storage is not configured on this server"),
    };

    let started = Instant::now();
    let asset = match generate_avatar_asset(request.clone()).await {
        Ok(asset) => asset,
        Err(response) => return response,
    };
    let elapsed = started.elapsed();
    state.metrics.observe_generation(elapsed);
    state
        .observability
        .record_avatar_render("avatar-link", &request, elapsed);
    state
        .metrics
        .avatar_link_total
        .fetch_add(1, Ordering::Relaxed);

    match tokio::time::timeout(
        Duration::from_millis(STORAGE_TIMEOUT_MS),
        storage.store_and_sign(&asset, &state.metrics),
    )
    .await
    {
        Ok(Ok(signed)) => (
            StatusCode::OK,
            Json(AvatarLinkResponse {
                object_key: signed.object_key,
                signed_url: signed.signed_url,
                expires_in_seconds: storage.presign_ttl.as_secs(),
                content_type: asset.content_type.to_string(),
                cache_key: sha256_hex(&asset.cache_key),
            }),
        )
            .into_response(),
        Ok(Err(error)) => internal_error(error),
        Err(_) => request_timeout("object storage timed out"),
    }
}

async fn generate_avatar_asset(request: AvatarRequest) -> Result<AvatarAsset, Response> {
    let render_permit = RENDER_SLOTS
        .clone()
        .try_acquire_owned()
        .map_err(|_| server_busy())?;
    let render = tokio::task::spawn_blocking(move || {
        let _render_permit = render_permit;
        build_avatar_asset(&request)
    });
    match tokio::time::timeout(Duration::from_millis(AVATAR_TIMEOUT_MS), render).await {
        Ok(Ok(Ok(asset))) => Ok(asset),
        Ok(Ok(Err(message))) => Err(bad_request(&message)),
        Ok(Err(error)) => Err(internal_error(error)),
        Err(_) => Err(request_timeout("avatar generation timed out")),
    }
}

fn build_avatar_asset(request: &AvatarRequest) -> Result<AvatarAsset, String> {
    let identity = request.identity.trim();
    validate_identity(identity)?;
    validate_namespace_component("tenant", &request.namespace_tenant)?;
    validate_namespace_component("style_version", &request.namespace_style)?;

    if !(MIN_SIZE..=MAX_SIZE).contains(&request.size) {
        return Err("size must be between 64 and 1024".to_string());
    }

    let spec = AvatarSpec::new(request.size, request.size, 0)
        .map_err(|_| INVALID_AVATAR_RENDER_MESSAGE.to_string())?;
    let style = request.style_options();
    let namespace = AvatarNamespace::new(&request.namespace_tenant, &request.namespace_style)
        .map_err(|_| INVALID_NAMESPACE_MESSAGE.to_string())?;
    let identity_options = AvatarIdentityOptions::new(namespace);
    let accessory = request.effective_accessory();
    let expression = request.effective_expression();
    let identity_cache_key = sha256_hex(identity);
    let cache_key = format!(
        "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
        request.namespace_tenant,
        request.namespace_style,
        DEFAULT_HASH_ALGORITHM,
        identity_cache_key,
        request.kind,
        request.background,
        accessory,
        request.color,
        expression,
        request.shape,
        request.format,
        request.size
    );

    let body = encode_avatar_style_with_identity_options(
        spec,
        identity_options,
        identity,
        AvatarOutputFormat::WebP,
        style,
    )
    .map_err(|_| INVALID_AVATAR_RENDER_MESSAGE.to_string())?;

    Ok(AvatarAsset {
        body,
        content_type: "image/webp",
        cache_key,
        object_key: object_key_for(request, identity),
    })
}

fn cache_headers(etag: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400, s-maxage=31536000, immutable"),
    );
    headers.insert(
        HeaderName::cdn_cache_control(),
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    headers.insert(
        HeaderName::cloudflare_cache_control(),
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(etag).unwrap_or_else(|_| HeaderValue::from_static("\"avatar\"")),
    );
    headers
}

fn etag_for(cache_key: &str) -> String {
    format!("\"{}\"", sha256_hex(cache_key))
}

fn sha256_hex(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

fn object_key_for(request: &AvatarRequest, identity: &str) -> String {
    let accessory = request.effective_accessory();
    let expression = request.effective_expression();
    let digest = Sha256::digest(
        format!(
            "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
            request.namespace_tenant,
            request.namespace_style,
            DEFAULT_HASH_ALGORITHM,
            identity,
            request.kind,
            request.background,
            accessory,
            request.color,
            expression,
            request.shape,
            request.format,
            request.size
        )
        .as_bytes(),
    );
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    format!(
        "{}/{}/{}/{}/{}/{}/{}/{}/{}/{}/{}.{}",
        request.namespace_tenant,
        request.namespace_style,
        DEFAULT_HASH_ALGORITHM,
        request.kind.as_str(),
        request.background.as_str(),
        accessory.as_str(),
        request.color.as_str(),
        expression.as_str(),
        request.shape.as_str(),
        request.size,
        encoded,
        request.format.as_str()
    )
}

fn validate_identity(identity: &str) -> Result<(), String> {
    if identity.is_empty() {
        return Err("missing identity".to_string());
    }
    if identity.len() > MAX_ID_BYTES {
        return Err(format!(
            "identity must be at most {MAX_ID_BYTES} bytes; send a stable internal id or one-way hash"
        ));
    }
    Ok(())
}

fn validate_namespace_component(name: &str, value: &str) -> Result<(), String> {
    if !is_valid_namespace_component(value) {
        return Err(format!(
            "{name} must be 1-{MAX_NAMESPACE_COMPONENT_BYTES} ASCII letters, digits, hyphens, or underscores"
        ));
    }
    Ok(())
}

fn validate_hash_algorithm(value: Option<&str>) -> Result<(), String> {
    match value.map(str::trim) {
        Some(raw) if !raw.is_empty() && !raw.eq_ignore_ascii_case(DEFAULT_HASH_ALGORITHM) => {
            Err(INVALID_HASH_ALGORITHM_MESSAGE.to_string())
        }
        _ => Ok(()),
    }
}

fn is_valid_namespace_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_NAMESPACE_COMPONENT_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn clamp_s3_presign_ttl_seconds(ttl: u64) -> u64 {
    ttl.clamp(MIN_S3_PRESIGN_TTL_SECONDS, MAX_S3_PRESIGN_TTL_SECONDS)
}

fn normalize_s3_prefix(prefix: &str) -> Result<String, String> {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        return Err("must not be empty".to_string());
    }
    if !prefix
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'/')
    {
        return Err(
            "must contain only ASCII letters, digits, hyphens, underscores, or slashes".to_string(),
        );
    }
    if prefix.split('/').any(str::is_empty) {
        return Err("must not contain empty path segments".to_string());
    }

    Ok(prefix.to_string())
}

fn apply_no_store(headers: &mut HeaderMap) {
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
}

fn bad_request(message: &str) -> Response {
    let mut response = (StatusCode::BAD_REQUEST, message.to_string()).into_response();
    apply_no_store(response.headers_mut());
    response
}

fn internal_error(error: impl std::fmt::Display) -> Response {
    tracing::error!(error = %error, "avatar generation failed");
    let mut response = (StatusCode::INTERNAL_SERVER_ERROR, INTERNAL_ERROR_MESSAGE).into_response();
    apply_no_store(response.headers_mut());
    response
}

fn request_timeout(message: &str) -> Response {
    let mut response = (StatusCode::REQUEST_TIMEOUT, message.to_string()).into_response();
    apply_no_store(response.headers_mut());
    response
}

fn server_busy() -> Response {
    let mut response = (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER, "1")],
        "server busy, retry shortly",
    )
        .into_response();
    apply_no_store(response.headers_mut());
    response
}

fn draw_rect(image: &mut RgbaImage, x: u32, y: u32, width: u32, height: u32, color: Rgba<u8>) {
    for yy in y..(y + height).min(image.height()) {
        for xx in x..(x + width).min(image.width()) {
            image.put_pixel(xx, yy, color);
        }
    }
}

fn draw_circle(image: &mut RgbaImage, cx: i32, cy: i32, radius: i32, color: Rgba<u8>) {
    if radius < 0 {
        return;
    }
    for y in -radius..=radius {
        for x in -radius..=radius {
            if is_inside_circle(x, y, radius) {
                let px = cx + x;
                let py = cy + y;
                if px >= 0 && py >= 0 && (px as u32) < image.width() && (py as u32) < image.height()
                {
                    image.put_pixel(px as u32, py as u32, color);
                }
            }
        }
    }
}

fn is_inside_circle(x: i32, y: i32, radius: i32) -> bool {
    if radius < 0 {
        return false;
    }
    let x_squared = i64::from(x) * i64::from(x);
    let y_squared = i64::from(y) * i64::from(y);
    let radius_squared = i64::from(radius) * i64::from(radius);
    x_squared + y_squared <= radius_squared
}

fn overlay(
    canvas: &mut RgbaImage,
    image: &RgbaImage,
    x: u32,
    y: u32,
) -> Result<(), image::ImageError> {
    canvas.copy_from(image, x, y)
}

fn shared_page_styles() -> &'static str {
    r#"
    :root {
      --bg: #fbf6ee;
      --panel: rgba(255,255,255,0.86);
      --ink: #1f2933;
      --muted: #52606d;
      --line: rgba(31, 41, 51, 0.08);
      --accent: #d97a42;
      --accent-strong: #b85a25;
      --surface: rgba(255,255,255,0.74);
      font-family: "IBM Plex Sans", "Segoe UI", sans-serif;
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      min-height: 100vh;
      background:
        radial-gradient(circle at top left, rgba(255, 214, 170, 0.95), transparent 26%),
        radial-gradient(circle at bottom right, rgba(217, 122, 66, 0.18), transparent 30%),
        linear-gradient(135deg, #fbf6ee, #f2ece4);
      color: var(--ink);
      padding: 32px 20px;
    }
    main {
      width: min(1180px, 100%);
      margin: 0 auto;
      background: var(--panel);
      border: 1px solid var(--line);
      border-radius: 28px;
      box-shadow: 0 24px 70px rgba(75, 48, 25, 0.14);
      overflow: hidden;
    }
    .site-nav {
      display: flex;
      justify-content: space-between;
      align-items: center;
      gap: 16px;
      padding: 20px 28px;
      border-bottom: 1px solid var(--line);
      background: rgba(255,255,255,0.5);
    }
    .brand {
      font-weight: 800;
      letter-spacing: 0;
      color: var(--ink);
      text-decoration: none;
    }
    .nav-links, .footer-links {
      display: flex;
      flex-wrap: wrap;
      gap: 12px;
      align-items: center;
    }
    .nav-links a, .footer-links a, .inline-link, .language-switcher summary {
      color: var(--accent-strong);
      text-decoration: none;
      font-weight: 700;
    }
    .nav-links a:hover, .footer-links a:hover, .inline-link:hover, .language-switcher summary:hover {
      text-decoration: underline;
    }
    .language-switcher {
      position: relative;
    }
    .language-switcher summary {
      cursor: pointer;
      list-style: none;
    }
    .language-switcher summary::-webkit-details-marker {
      display: none;
    }
    .language-switcher summary span::after {
      content: " ▾";
      font-size: 0.82em;
    }
    .language-menu {
      position: absolute;
      inset-inline-end: 0;
      bottom: calc(100% + 8px);
      min-width: 190px;
      padding: 8px;
      background: white;
      border: 1px solid var(--line);
      border-radius: 14px;
      box-shadow: 0 18px 40px rgba(82,96,109,0.16);
      display: grid;
      gap: 4px;
      z-index: 10;
    }
    .language-menu a {
      padding: 8px 10px;
      border-radius: 10px;
      color: var(--ink);
      text-decoration: none;
      white-space: nowrap;
    }
    .language-menu a:hover, .language-menu a[aria-current="true"] {
      background: rgba(217, 122, 66, 0.12);
      color: var(--accent-strong);
      text-decoration: none;
    }
    .page {
      padding: 36px;
      display: grid;
      gap: 18px;
    }
    .eyebrow {
      text-transform: uppercase;
      color: var(--accent);
      font-weight: 700;
      font-size: 0.8rem;
      letter-spacing: 0;
    }
    h1 {
      font-size: clamp(2.2rem, 6vw, 4.4rem);
      line-height: 0.95;
      margin: 8px 0 8px;
      letter-spacing: 0;
      max-width: 12ch;
    }
    h2 {
      margin: 12px 0 8px;
      font-size: 1.2rem;
    }
    p, li {
      color: var(--muted);
      line-height: 1.7;
      font-size: 1rem;
    }
    ul {
      margin: 0;
      padding-inline-start: 20px;
    }
    .lead {
      max-width: 70ch;
      margin: 0;
    }
    .content-grid {
      display: grid;
      gap: 18px;
      grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
    }
    .card {
      padding: 20px;
      background: white;
      border: 1px solid var(--line);
      border-radius: 22px;
      display: grid;
      gap: 10px;
    }
    pre {
      margin: 0;
      padding: 14px;
      background: white;
      border: 1px solid var(--line);
      border-radius: 18px;
      overflow: auto;
      font-size: 0.94rem;
    }
    code {
      font-family: "IBM Plex Mono", monospace;
    }
    pre, code, .url-text {
      direction: ltr;
      text-align: left;
      unicode-bidi: plaintext;
    }
    .site-footer {
      padding: 24px 28px 28px;
      border-top: 1px solid var(--line);
      display: grid;
      gap: 10px;
      background: rgba(255,255,255,0.52);
    }
    .footer-copy {
      color: var(--muted);
      font-size: 0.95rem;
    }
    @media (max-width: 860px) {
      .site-nav {
        align-items: start;
        flex-direction: column;
      }
      .language-menu {
        inset-inline-start: 0;
        inset-inline-end: auto;
      }
      .page {
        padding: 24px;
      }
    }
    "#
}

fn render_footer_html(i18n: I18n, path: &str) -> String {
    format!(
        r#"<footer class="site-footer">
  <div class="footer-links">
    <a href="{help_href}">{help}</a>
    <a href="{docs_href}">{docs}</a>
    <a href="{terms_href}">{terms}</a>
    <a href="{privacy_href}">{privacy}</a>
    <a href="{repo}" target="_blank" rel="noreferrer">{repository}</a>
    <a href="{crate_url}" target="_blank" rel="noreferrer">{rust_crate}</a>
    {language_switcher}
  </div>
  <div class="footer-copy">
    {copy}
  </div>
</footer>"#,
        help_href = localized_path(i18n.locale, "help"),
        docs_href = localized_path(i18n.locale, "docs"),
        terms_href = localized_path(i18n.locale, "terms"),
        privacy_href = localized_path(i18n.locale, "privacy"),
        help = i18n.t_attr("nav.help", "Help"),
        docs = i18n.t_attr("nav.docs", "Docs"),
        terms = i18n.t_attr("nav.terms", "Terms"),
        privacy = i18n.t_attr("nav.privacy", "Privacy"),
        repository = i18n.t_attr("nav.repository", "Repository"),
        rust_crate = i18n.t_attr("nav.rust_crate", "Rust Crate"),
        language_switcher = render_language_switcher(i18n, path),
        copy = i18n.t_attr("footer.copy", "hashavatar.app is a deterministic avatar API and demo service built on the open-source hashavatar Rust crate."),
        repo = REPOSITORY_URL,
        crate_url = CRATE_URL,
    )
}

fn render_language_switcher(i18n: I18n, path: &str) -> String {
    let active = i18n.locale;
    let active_label = format!("{} {}", active.flag, active.display_name);
    let links = locales()
        .iter()
        .map(|locale| {
            let label = escape_html_attribute(&format!("{} {}", locale.flag, locale.display_name));
            let href = escape_html_attribute(&localized_path(locale, path));
            let current = if locale.locale_id == active.locale_id {
                r#" aria-current="true""#
            } else {
                ""
            };
            format!(r#"<a href="{href}"{current}>{label}</a>"#)
        })
        .collect::<Vec<_>>()
        .join("");

    format!(
        r#"<details class="language-switcher">
      <summary aria-label="{label}"><span>{active}</span></summary>
      <div class="language-menu">{links}</div>
    </details>"#,
        label = i18n.t_attr("language.selector_label", "Language"),
        active = escape_html_attribute(&active_label),
        links = links,
    )
}

fn escape_html_attribute(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn selected_attr(selected: bool) -> &'static str {
    if selected { " selected" } else { "" }
}

fn disabled_attr(disabled: bool) -> &'static str {
    if disabled { " disabled" } else { "" }
}

fn avatar_kind_label(kind: AvatarKind) -> &'static str {
    match kind {
        AvatarKind::Cat => "Cat",
        AvatarKind::Dog => "Dog",
        AvatarKind::Robot => "Robot",
        AvatarKind::Fox => "Fox",
        AvatarKind::Alien => "Alien",
        AvatarKind::Monster => "Monster",
        AvatarKind::Ghost => "Ghost",
        AvatarKind::Slime => "Slime",
        AvatarKind::Bird => "Bird",
        AvatarKind::Wizard => "Wizard",
        AvatarKind::Skull => "Skull",
        AvatarKind::Paws => "Paws",
        AvatarKind::Planet => "Planet",
        AvatarKind::Rocket => "Rocket",
        AvatarKind::Mushroom => "Mushroom",
        AvatarKind::Cactus => "Cactus",
        AvatarKind::Frog => "Frog",
        AvatarKind::Panda => "Panda",
        AvatarKind::Cupcake => "Cupcake",
        AvatarKind::Pizza => "Pizza",
        AvatarKind::Icecream => "Ice Cream",
        AvatarKind::Octopus => "Octopus",
        AvatarKind::Knight => "Knight",
        AvatarKind::Bear => "Bear",
        AvatarKind::Penguin => "Penguin",
        AvatarKind::Dragon => "Dragon",
        AvatarKind::Ninja => "Ninja",
        AvatarKind::Astronaut => "Astronaut",
        AvatarKind::Diamond => "Diamond",
        AvatarKind::CoffeeCup => "Coffee Cup",
        AvatarKind::Shield => "Shield",
    }
}

fn background_label(background: AvatarBackground) -> &'static str {
    match background {
        AvatarBackground::Themed => "Themed",
        AvatarBackground::White => "White",
        AvatarBackground::Black => "Black",
        AvatarBackground::Dark => "Dark",
        AvatarBackground::Light => "Light",
        AvatarBackground::Transparent => "Transparent",
        AvatarBackground::PolkaDot => "Polka Dot",
        AvatarBackground::Striped => "Striped",
        AvatarBackground::Checkerboard => "Checkerboard",
        AvatarBackground::Grid => "Grid",
        AvatarBackground::Sunrise => "Sunrise",
        AvatarBackground::Ocean => "Ocean",
        AvatarBackground::Starry => "Starry",
    }
}

fn accessory_label(accessory: AvatarAccessory) -> &'static str {
    match accessory {
        AvatarAccessory::None => "None",
        AvatarAccessory::Glasses => "Glasses",
        AvatarAccessory::Hat => "Hat",
        AvatarAccessory::Headphones => "Headphones",
        AvatarAccessory::Crown => "Crown",
        AvatarAccessory::Bowtie => "Bowtie",
        AvatarAccessory::Eyepatch => "Eyepatch",
        AvatarAccessory::Scarf => "Scarf",
        AvatarAccessory::Halo => "Halo",
        AvatarAccessory::Horns => "Horns",
    }
}

fn color_label(color: AvatarColor) -> &'static str {
    match color {
        AvatarColor::Default => "Default",
        AvatarColor::NeonMint => "Neon Mint",
        AvatarColor::PastelPink => "Pastel Pink",
        AvatarColor::Crimson => "Crimson",
        AvatarColor::Gold => "Gold",
        AvatarColor::DeepSeaBlue => "Deep Sea Blue",
    }
}

fn expression_label(expression: AvatarExpression) -> &'static str {
    match expression {
        AvatarExpression::Default => "Default",
        AvatarExpression::Happy => "Happy",
        AvatarExpression::Grumpy => "Grumpy",
        AvatarExpression::Surprised => "Surprised",
        AvatarExpression::Sleepy => "Sleepy",
        AvatarExpression::Winking => "Winking",
        AvatarExpression::Cool => "Cool",
        AvatarExpression::Crying => "Crying",
    }
}

fn shape_label(shape: AvatarShape) -> &'static str {
    match shape {
        AvatarShape::Square => "Square",
        AvatarShape::Circle => "Circle",
        AvatarShape::Squircle => "Squircle",
        AvatarShape::Hexagon => "Hexagon",
        AvatarShape::Octagon => "Octagon",
    }
}

fn kind_options_html(selected: AvatarKind) -> String {
    AvatarKind::ALL
        .iter()
        .copied()
        .map(|kind| {
            format!(
                r#"<option value="{value}" data-identity="{value}@hashavatar.app" data-supports-layers="{supports_layers}"{selected}>{label}</option>"#,
                value = kind.as_str(),
                label = avatar_kind_label(kind),
                supports_layers = kind.supports_face_layers(),
                selected = selected_attr(kind == selected),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn background_options_html(selected: AvatarBackground) -> String {
    AvatarBackground::ALL
        .iter()
        .copied()
        .map(|background| {
            format!(
                r#"<option value="{value}"{selected}>{label}</option>"#,
                value = background.as_str(),
                label = background_label(background),
                selected = selected_attr(background == selected),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn accessory_options_html(selected: AvatarAccessory) -> String {
    AvatarAccessory::ALL
        .iter()
        .copied()
        .map(|accessory| {
            format!(
                r#"<option value="{value}"{selected}>{label}</option>"#,
                value = accessory.as_str(),
                label = accessory_label(accessory),
                selected = selected_attr(accessory == selected),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn color_options_html(selected: AvatarColor) -> String {
    AvatarColor::ALL
        .iter()
        .copied()
        .map(|color| {
            format!(
                r#"<option value="{value}"{selected}>{label}</option>"#,
                value = color.as_str(),
                label = color_label(color),
                selected = selected_attr(color == selected),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn expression_options_html(selected: AvatarExpression) -> String {
    AvatarExpression::ALL
        .iter()
        .copied()
        .map(|expression| {
            format!(
                r#"<option value="{value}"{selected}>{label}</option>"#,
                value = expression.as_str(),
                label = expression_label(expression),
                selected = selected_attr(expression == selected),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn shape_options_html(selected: AvatarShape) -> String {
    AvatarShape::ALL
        .iter()
        .copied()
        .map(|shape| {
            format!(
                r#"<option value="{value}"{selected}>{label}</option>"#,
                value = shape.as_str(),
                label = shape_label(shape),
                selected = selected_attr(shape == selected),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_nav_html(i18n: I18n) -> String {
    format!(
        r#"<div class="site-nav">
      <a class="brand" href="{home_href}">{site_name}</a>
      <div class="nav-links">
        <a href="{help_href}">{help}</a>
        <a href="{docs_href}">{docs}</a>
        <a href="{terms_href}">{terms}</a>
        <a href="{privacy_href}">{privacy}</a>
        <a href="{repo}" target="_blank" rel="noreferrer">{repository}</a>
        <a href="{crate_url}" target="_blank" rel="noreferrer">{rust_crate}</a>
      </div>
    </div>"#,
        home_href = localized_path(i18n.locale, ""),
        help_href = localized_path(i18n.locale, "help"),
        docs_href = localized_path(i18n.locale, "docs"),
        terms_href = localized_path(i18n.locale, "terms"),
        privacy_href = localized_path(i18n.locale, "privacy"),
        help = i18n.t_attr("nav.help", "Help"),
        docs = i18n.t_attr("nav.docs", "Docs"),
        terms = i18n.t_attr("nav.terms", "Terms"),
        privacy = i18n.t_attr("nav.privacy", "Privacy"),
        repository = i18n.t_attr("nav.repository", "Repository"),
        rust_crate = i18n.t_attr("nav.rust_crate", "Rust Crate"),
        repo = REPOSITORY_URL,
        crate_url = CRATE_URL,
        site_name = SITE_NAME,
    )
}

#[derive(Serialize)]
struct PresetExample {
    label: &'static str,
    id: &'static str,
    kind: &'static str,
    background: &'static str,
    format: &'static str,
    size: &'static str,
}

fn preset_examples() -> Vec<PresetExample> {
    AvatarKind::ALL
        .iter()
        .copied()
        .map(|kind| PresetExample {
            label: avatar_kind_label(kind),
            id: match kind {
                AvatarKind::Icecream => "icecream@hashavatar.app",
                _ => kind.as_str(),
            },
            kind: kind.as_str(),
            background: match kind {
                AvatarKind::Dog
                | AvatarKind::Robot
                | AvatarKind::Slime
                | AvatarKind::Wizard
                | AvatarKind::Paws
                | AvatarKind::Penguin
                | AvatarKind::Astronaut
                | AvatarKind::CoffeeCup => "white",
                AvatarKind::Panda | AvatarKind::Knight | AvatarKind::Bear => "light",
                AvatarKind::Ghost | AvatarKind::Skull => "dark",
                _ => "themed",
            },
            format: "webp",
            size: "256",
        })
        .map(|mut preset| {
            preset.id = match preset.kind {
                "cat" => "cat@hashavatar.app",
                "dog" => "dog@hashavatar.app",
                "robot" => "robot@hashavatar.app",
                "fox" => "fox@hashavatar.app",
                "alien" => "alien@hashavatar.app",
                "monster" => "monster@hashavatar.app",
                "ghost" => "ghost@hashavatar.app",
                "slime" => "slime@hashavatar.app",
                "bird" => "bird@hashavatar.app",
                "wizard" => "wizard@hashavatar.app",
                "skull" => "skull@hashavatar.app",
                "paws" => "paws@hashavatar.app",
                "planet" => "planet@hashavatar.app",
                "rocket" => "rocket@hashavatar.app",
                "mushroom" => "mushroom@hashavatar.app",
                "cactus" => "cactus@hashavatar.app",
                "frog" => "frog@hashavatar.app",
                "panda" => "panda@hashavatar.app",
                "cupcake" => "cupcake@hashavatar.app",
                "pizza" => "pizza@hashavatar.app",
                "icecream" => "icecream@hashavatar.app",
                "octopus" => "octopus@hashavatar.app",
                "knight" => "knight@hashavatar.app",
                "bear" => "bear@hashavatar.app",
                "penguin" => "penguin@hashavatar.app",
                "dragon" => "dragon@hashavatar.app",
                "ninja" => "ninja@hashavatar.app",
                "astronaut" => "astronaut@hashavatar.app",
                "diamond" => "diamond@hashavatar.app",
                "coffee-cup" => "coffee-cup@hashavatar.app",
                "shield" => "shield@hashavatar.app",
                _ => DEFAULT_ID,
            };
            preset
        })
        .collect()
}

fn preset_examples_json() -> String {
    static PRESET_EXAMPLES_JSON: LazyLock<String> = LazyLock::new(|| {
        serde_json::to_string(&preset_examples()).unwrap_or_else(|error| {
            tracing::error!(%error, "failed to serialize preset examples");
            "[]".to_string()
        })
    });

    PRESET_EXAMPLES_JSON.to_string()
}

fn render_meta_tags(
    title: &str,
    description: &str,
    path: &str,
    csp_nonce: &CspNonce,
    i18n: I18n,
) -> String {
    let canonical = format!("{SITE_URL}{}", localized_path(i18n.locale, path));
    let preview_image = format!(
        "{site}/og.png?id=hashavatar.app&kind=monster",
        site = SITE_URL
    );
    let full_title = format!("{title} · {SITE_NAME}");

    format!(
        r#"<title>{title}</title>
  <meta name="description" content="{description}" />
  <meta name="robots" content="index,follow,max-image-preview:large,max-snippet:-1,max-video-preview:-1" />
  <link rel="canonical" href="{canonical}" />
  <link rel="icon" href="/favicon.svg" type="image/svg+xml" />
  <link rel="manifest" href="/site.webmanifest" />
  <meta property="og:type" content="website" />
  <meta property="og:site_name" content="{site_name}" />
  <meta property="og:title" content="{title}" />
  <meta property="og:description" content="{description}" />
  <meta property="og:url" content="{canonical}" />
  <meta property="og:image" content="{image}" />
  <meta property="og:image:alt" content="Procedural avatar preview from hashavatar.app" />
  <meta name="twitter:card" content="summary_large_image" />
  <meta name="twitter:title" content="{title}" />
  <meta name="twitter:description" content="{description}" />
  <meta name="twitter:image" content="{image}" />
  {json_ld}"#,
        title = escape_html_attribute(&full_title),
        description = escape_html_attribute(description),
        canonical = escape_html_attribute(&canonical),
        image = escape_html_attribute(&preview_image),
        site_name = escape_html_attribute(SITE_NAME),
        json_ld = render_json_ld(&full_title, description, &canonical, csp_nonce),
    )
}

fn json_script_string(value: &str, fallback: &str) -> String {
    serde_json::to_string(value)
        .or_else(|_| serde_json::to_string(fallback))
        .unwrap_or_else(|_| "\"\"".to_string())
        .replace("</", "<\\/")
        .replace("<!--", "<\\u0021--")
}

fn render_json_ld(title: &str, description: &str, canonical: &str, csp_nonce: &CspNonce) -> String {
    let title = json_script_string(title, "hashavatar.app");
    let description = json_script_string(description, "Deterministic avatar API");
    let canonical = json_script_string(canonical, &format!("{SITE_URL}/"));
    let site_url = json_script_string(SITE_URL, SITE_URL);
    let search_target = json_script_string(
        &format!("{SITE_URL}/?id={{search_term_string}}"),
        &format!("{SITE_URL}/?id={{search_term_string}}"),
    );
    let nonce = escape_html_attribute(csp_nonce.as_str());

    format!(
        r#"<script nonce="{nonce}" type="application/ld+json">{{
  "@context": "https://schema.org",
  "@type": "WebSite",
  "name": {title},
  "url": {site_url},
  "description": {description},
  "potentialAction": {{
    "@type": "SearchAction",
    "target": {search_target},
    "query-input": "required name=search_term_string"
  }}
}}</script>
<script nonce="{nonce}" type="application/ld+json">{{
  "@context": "https://schema.org",
  "@type": "WebPage",
  "name": {title},
  "url": {canonical},
  "description": {description},
  "isPartOf": {{
    "@type": "WebSite",
    "name": "hashavatar.app",
    "url": {site_url}
  }}
}}</script>"#,
        title = title,
        description = description,
        canonical = canonical,
        site_url = site_url,
        search_target = search_target,
        nonce = nonce,
    )
}

fn render_telemetry_script(csp_nonce: &CspNonce, route: &str, section: &str, i18n: I18n) -> String {
    let nonce = escape_html_attribute(csp_nonce.as_str());
    let route = json_script_string(route, "/");
    let section = json_script_string(section, "home");
    let locale = json_script_string(i18n.locale_id(), DEFAULT_LOCALE_ID);
    let repository_url = json_script_string(REPOSITORY_URL, REPOSITORY_URL);
    let crate_url = json_script_string(CRATE_URL, CRATE_URL);

    format!(
        r#"<script nonce="{nonce}">
    (function () {{
      const pageRoute = {route};
      const pageSection = {section};
      const pageLocale = {locale};
      const repositoryUrl = {repository_url};
      const crateUrl = {crate_url};
      let visibleStartedAt = Date.now();
      let visibleMillis = 0;

      function sendJson(endpoint, payload) {{
        try {{
          const body = JSON.stringify(payload);
          if (navigator.sendBeacon) {{
            const blob = new Blob([body], {{ type: "application/json" }});
            if (navigator.sendBeacon(endpoint, blob)) {{
              return;
            }}
          }}
          fetch(endpoint, {{
            method: "POST",
            headers: {{ "content-type": "application/json" }},
            body,
            credentials: "same-origin",
            keepalive: true,
          }}).catch(() => {{}});
        }} catch (_) {{}}
      }}

      function flushVisibleTime() {{
        const activeMillis = document.visibilityState === "visible"
          ? Math.max(0, Date.now() - visibleStartedAt)
          : 0;
        const seconds = Math.min(86400, Math.round((visibleMillis + activeMillis) / 1000));
        if (seconds > 0) {{
          sendJson("/telemetry/page-visible", {{
            route: pageRoute,
            section: pageSection,
            locale: pageLocale,
            seconds,
          }});
        }}
        visibleMillis = 0;
        visibleStartedAt = Date.now();
      }}

      document.addEventListener("visibilitychange", () => {{
        if (document.visibilityState === "hidden") {{
          visibleMillis += Math.max(0, Date.now() - visibleStartedAt);
          flushVisibleTime();
        }} else {{
          visibleStartedAt = Date.now();
        }}
      }});
      window.addEventListener("pagehide", flushVisibleTime);

      document.addEventListener("click", (event) => {{
        const link = event.target.closest && event.target.closest("a[href]");
        if (!link) {{
          return;
        }}
        const href = link.href || "";
        if (href.startsWith(repositoryUrl)) {{
          sendJson("/telemetry/click", {{ kind: "github", target: "repository", locale: pageLocale }});
        }} else if (href.startsWith(crateUrl)) {{
          sendJson("/telemetry/click", {{ kind: "outbound", target: "crate", locale: pageLocale }});
        }}
      }});

      window.hashavatarTelemetry = {{
        click(kind, target) {{
          sendJson("/telemetry/click", {{ kind, target, locale: pageLocale }});
        }},
        avatar(payload) {{
          sendJson("/telemetry/avatar-generate", Object.assign({{ locale: pageLocale }}, payload));
        }},
      }};
    }})();
  </script>"#,
        nonce = nonce,
        route = route,
        section = section,
        locale = locale,
        repository_url = repository_url,
        crate_url = crate_url,
    )
}

struct PageHtmlParams<'a> {
    page_title: String,
    description: String,
    path: &'a str,
    eyebrow: String,
    lead: String,
    body: String,
    csp_nonce: &'a CspNonce,
    telemetry_enabled: bool,
    i18n: I18n,
}

fn render_page_html(params: PageHtmlParams<'_>) -> String {
    let PageHtmlParams {
        page_title,
        description,
        path,
        eyebrow,
        lead,
        body,
        csp_nonce,
        telemetry_enabled,
        i18n,
    } = params;
    let nonce = escape_html_attribute(csp_nonce.as_str());
    let telemetry_script = if telemetry_enabled {
        render_telemetry_script(csp_nonce, path, section_for_path(path), i18n)
    } else {
        String::new()
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="{html_lang}" dir="{dir}">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  {meta_tags}
  <style nonce="{nonce}">{styles}</style>
</head>
<body>
  <main>
    {nav}
    <section class="page">
      <div class="eyebrow">{eyebrow}</div>
      <h1>{page_title}</h1>
      <p class="lead">{lead}</p>
      {body}
    </section>
    {footer}
  </main>
  {telemetry_script}
</body>
</html>"#,
        meta_tags = render_meta_tags(&page_title, &description, path, csp_nonce, i18n),
        styles = shared_page_styles(),
        nonce = nonce,
        html_lang = escape_html_attribute(i18n.html_lang()),
        dir = escape_html_attribute(i18n.dir()),
        nav = render_nav_html(i18n),
        eyebrow = eyebrow,
        page_title = page_title,
        lead = lead,
        body = body,
        footer = render_footer_html(i18n, path),
        telemetry_script = telemetry_script,
    )
}

fn render_index_html(
    csp_nonce: &CspNonce,
    storage_links_enabled: bool,
    telemetry_enabled: bool,
    i18n: I18n,
) -> String {
    let description = i18n.t("hero.description", "Deterministic procedural avatars for opaque user ids, stable usernames, and one-way hashes. Generate 31 avatar families as WebP images.");
    let nonce = escape_html_attribute(csp_nonce.as_str());
    let telemetry_script = if telemetry_enabled {
        render_telemetry_script(csp_nonce, "/", "home", i18n)
    } else {
        String::new()
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="{html_lang}" dir="{dir}">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  {meta_tags}
  <style nonce="{nonce}">
    {styles}
    .hero {{
      display: grid;
      grid-template-columns: 1.1fr 0.9fr;
    }}
    .copy, .preview {{ padding: 36px; }}
    .copy {{ border-inline-end: 1px solid var(--line); }}
    h1 {{
      font-size: clamp(2.2rem, 6vw, 4.4rem);
      line-height: 0.95;
      margin: 12px 0 16px;
      letter-spacing: 0;
      max-width: 10ch;
    }}
    p {{
      color: var(--muted);
      line-height: 1.65;
      margin: 0 0 16px;
      max-width: 60ch;
    }}
    .eyebrow {{
      text-transform: uppercase;
      color: var(--accent);
      font-weight: 700;
      font-size: 0.8rem;
      letter-spacing: 0;
    }}
    .generator {{
      margin-top: 26px;
      display: grid;
      gap: 16px;
    }}
    .field-grid {{
      display: grid;
      grid-template-columns: 1fr 1fr;
      gap: 14px;
    }}
    .field-grid.full {{
      grid-template-columns: 1fr;
    }}
    label {{
      display: block;
      margin-bottom: 8px;
      font-size: 0.92rem;
      font-weight: 700;
      color: var(--ink);
    }}
    input, select {{
      width: 100%;
      border: 1px solid rgba(82, 96, 109, 0.18);
      background: rgba(255,255,255,0.95);
      color: var(--ink);
      border-radius: 16px;
      padding: 14px 16px;
      font: inherit;
      outline: none;
      transition: border-color 160ms ease, box-shadow 160ms ease, transform 160ms ease;
    }}
    input:focus, select:focus {{
      border-color: rgba(217, 122, 66, 0.65);
      box-shadow: 0 0 0 5px rgba(217, 122, 66, 0.12);
      transform: translateY(-1px);
    }}
    .actions {{
      display: flex;
      flex-wrap: wrap;
      gap: 12px;
    }}
    button, .button-link {{
      border: 0;
      border-radius: 16px;
      padding: 14px 18px;
      background: linear-gradient(180deg, #dd8750, #c96831);
      color: white;
      font: inherit;
      font-weight: 700;
      cursor: pointer;
      text-decoration: none;
      display: inline-flex;
      align-items: center;
      justify-content: center;
      min-height: 52px;
      box-shadow: 0 14px 28px rgba(201, 104, 49, 0.22);
    }}
    .button-link.secondary, button.secondary {{
      background: white;
      color: var(--ink);
      border: 1px solid var(--line);
      box-shadow: none;
    }}
    .url-panel {{
      padding: 16px;
      background: white;
      border: 1px solid var(--line);
      border-radius: 18px;
      display: grid;
      gap: 8px;
    }}
    .url-label {{
      font-size: 0.84rem;
      text-transform: uppercase;
      letter-spacing: 0;
      color: var(--accent-strong);
      font-weight: 700;
    }}
    .url-text {{
      overflow-wrap: anywhere;
      font-family: "IBM Plex Mono", monospace;
      font-size: 0.94rem;
      color: var(--ink);
    }}
    .preview {{
      display: grid;
      align-content: start;
      gap: 18px;
      background:
        radial-gradient(circle at center, rgba(255,255,255,0.74), rgba(255,255,255,0) 62%),
        linear-gradient(180deg, rgba(255,255,255,0.5), rgba(255,255,255,0.15));
    }}
    .panel {{
      width: min(320px, 100%);
      aspect-ratio: 1;
      border-radius: 28px;
      background: linear-gradient(180deg, rgba(255,255,255,0.95), rgba(255,255,255,0.74));
      box-shadow: inset 0 1px 0 rgba(255,255,255,0.8), 0 18px 40px rgba(82,96,109,0.12);
      display: grid;
      place-items: center;
      padding: 12px;
    }}
    img {{
      width: 100%;
      height: auto;
      display: block;
    }}
    .preview-meta {{
      width: 100%;
      padding: 16px;
      border-radius: 18px;
      border: 1px solid var(--line);
      background: var(--surface);
      color: var(--muted);
      display: grid;
      gap: 6px;
    }}
    .examples {{
      display: grid;
      gap: 14px;
      margin-top: 24px;
      width: 100%;
    }}
    .example-grid {{
      display: grid;
      grid-template-columns: repeat(3, minmax(0, 1fr));
      gap: 14px;
    }}
    .example-header {{
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 12px;
    }}
    .example-page {{
      color: var(--muted);
      font-size: 0.9rem;
      font-weight: 700;
    }}
    .example-card {{
      border: 1px solid var(--line);
      border-radius: 20px;
      background: rgba(255,255,255,0.74);
      padding: 14px;
      display: grid;
      gap: 10px;
      cursor: pointer;
      transition: transform 160ms ease, box-shadow 160ms ease;
    }}
    .example-card:hover {{
      transform: translateY(-2px);
      box-shadow: 0 16px 30px rgba(82,96,109,0.1);
    }}
    .example-card img {{
      border-radius: 16px;
      border: 1px solid var(--line);
      background: white;
    }}
    .example-title {{
      font-weight: 700;
      color: var(--ink);
    }}
    .example-controls {{
      display: flex;
      justify-content: center;
      gap: 10px;
      margin-top: 4px;
    }}
    .example-controls button {{
      min-height: 38px;
      padding: 8px 12px;
      border-radius: 999px;
      line-height: 1;
    }}
    pre {{
      margin: 0;
      padding: 14px;
      background: white;
      border: 1px solid var(--line);
      border-radius: 18px;
      overflow: auto;
      font-size: 0.94rem;
    }}
    code {{ font-family: "IBM Plex Mono", monospace; }}
    @media (max-width: 860px) {{
      .hero {{ grid-template-columns: 1fr; }}
      .copy {{ border-inline-end: 0; border-bottom: 1px solid var(--line); }}
      .field-grid {{ grid-template-columns: 1fr; }}
      .example-grid {{ grid-template-columns: repeat(2, minmax(0, 1fr)); }}
    }}
    @media (max-width: 560px) {{
      .example-grid {{ grid-template-columns: 1fr; }}
    }}
  </style>
</head>
<body>
  <main>
    {nav}
    <section class="hero">
      <div class="copy">
        <div class="eyebrow">{hero_eyebrow}</div>
        <h1>{hero_title}</h1>
        <p>{hero_description}</p>
        <p>{hero_privacy_tip}</p>

        <div class="generator">
          <div class="field-grid full">
            <div>
              <label for="identity">{identity_label}</label>
              <input id="identity" type="text" value="{id}" placeholder="cat@hashavatar.app" spellcheck="false" autocomplete="off" />
            </div>
          </div>

          <div class="field-grid">
            <div>
              <label for="tenant">{tenant_label}</label>
              <input id="tenant" type="text" value="{tenant}" placeholder="public" spellcheck="false" autocomplete="off" />
            </div>
            <div>
              <label for="style-version">{style_version_label}</label>
              <input id="style-version" type="text" value="{style_version}" placeholder="v2" spellcheck="false" autocomplete="off" />
            </div>
          </div>

          <div class="field-grid">
            <div>
              <label for="kind">{avatar_type_label}</label>
              <select id="kind">
                {kind_options}
              </select>
            </div>
            <div>
              <label for="background">{background_label}</label>
              <select id="background">
                {background_options}
              </select>
            </div>
          </div>

          <div class="field-grid">
            <div>
              <label for="accessory">{accessory_label}</label>
              <select id="accessory">
                {accessory_options}
              </select>
            </div>
            <div>
              <label for="color">{color_label}</label>
              <select id="color">
                {color_options}
              </select>
            </div>
          </div>

          <div class="field-grid">
            <div>
              <label for="expression">{expression_label}</label>
              <select id="expression">
                {expression_options}
              </select>
            </div>
            <div>
              <label for="shape">{shape_label}</label>
              <select id="shape">
                {shape_options}
              </select>
            </div>
          </div>

          <div class="field-grid full">
            <div>
              <label for="size">{size_label}</label>
              <select id="size">
                <option value="128">128</option>
                <option value="256" selected>256</option>
                <option value="320">320</option>
                <option value="512">512</option>
                <option value="1024">1024</option>
              </select>
            </div>
          </div>

          <div class="actions">
            <button id="copy-button" type="button">{copy_url_label}</button>
            <button id="copy-signed-button" type="button" class="secondary"{signed_disabled}>{copy_signed_label}</button>
            <a id="download-button" class="button-link" href="/v1/avatar?id={id}&algorithm=sha512&kind=cat&background=themed&format=webp&size=256" download="hashavatar.webp">{download_label}</a>
            <a id="open-button" class="button-link secondary" href="/v1/avatar?id={id}&algorithm=sha512&kind=cat&background=themed&format=webp&size=256" target="_blank" rel="noreferrer">{open_raw_label}</a>
          </div>

          <div class="url-panel">
            <div class="url-label">{direct_url_label}</div>
            <div id="avatar-url" class="url-text"></div>
          </div>

          <div class="url-panel">
            <div class="url-label">{signed_storage_label}</div>
            <div id="signed-url" class="url-text">{signed_unavailable}</div>
          </div>

          <div class="url-panel">
            <div class="url-label">{machine_api_label}</div>
            <div class="url-text"><a class="inline-link" href="/docs/openapi.json">/docs/openapi.json</a></div>
          </div>
        </div>
      </div>

      <div class="preview">
        <div class="panel">
          <img id="avatar-preview" src="/v1/avatar?id={id}&algorithm=sha512&kind=cat&background=themed&format=webp&size=256" alt="Generated avatar preview" />
        </div>
        <div class="preview-meta">
          <div><strong>{api_label}:</strong> <span id="api-mode">/v1/avatar</span></div>
          <div><strong>{storage_label}:</strong> {storage_text} <code>/v1/avatar/link</code></div>
          <div><strong>{cache_label}:</strong> {cache_text}</div>
          <div><strong>{tip_label}:</strong> {tip_text}</div>
        </div>

        <div class="examples">
          <div class="example-header">
            <div class="url-label">{preset_examples_label}</div>
            <div id="example-page" class="example-page"></div>
          </div>
          <div class="example-grid" id="example-grid">
          </div>
          <div class="example-controls">
            <button id="preset-prev" type="button" class="secondary" aria-label="Previous preset page">&larr;</button>
            <button id="preset-next" type="button" class="secondary" aria-label="Next preset page">&rarr;</button>
          </div>
        </div>
      </div>
    </section>
    {footer}
  </main>
  {telemetry_script}
  <script nonce="{nonce}">
    const identityEl = document.getElementById("identity");
    const tenantEl = document.getElementById("tenant");
    const styleVersionEl = document.getElementById("style-version");
    const kindEl = document.getElementById("kind");
    const backgroundEl = document.getElementById("background");
    const accessoryEl = document.getElementById("accessory");
    const colorEl = document.getElementById("color");
    const expressionEl = document.getElementById("expression");
    const shapeEl = document.getElementById("shape");
    const sizeEl = document.getElementById("size");
    const previewEl = document.getElementById("avatar-preview");
    const urlEl = document.getElementById("avatar-url");
    const signedUrlEl = document.getElementById("signed-url");
    const copyButton = document.getElementById("copy-button");
    const copySignedButton = document.getElementById("copy-signed-button");
    const downloadButton = document.getElementById("download-button");
    const openButton = document.getElementById("open-button");
    const exampleGrid = document.getElementById("example-grid");
    const examplePage = document.getElementById("example-page");
    const presetPrev = document.getElementById("preset-prev");
    const presetNext = document.getElementById("preset-next");
    const presetExamples = {preset_examples};
    const presetPageSize = {preset_page_size};
    const storageLinksEnabled = {storage_links_enabled};
    const presetIdentities = new Map(
      Array.from(kindEl.options).map((option) => [option.value, option.dataset.identity])
    );
    const styleLayerSupport = new Map(
      Array.from(kindEl.options).map((option) => [option.value, option.dataset.supportsLayers === "true"])
    );
    let presetPage = 0;
    let refreshTimer = 0;
    let presetRenderTimer = 0;

    function currentIdentity() {{
      return identityEl.value.trim() || "{id}";
    }}

    function selectedPresetIdentity() {{
      return presetIdentities.get(kindEl.value) || "{id}";
    }}

    function supportsStyleLayers(kind) {{
      return styleLayerSupport.get(kind) !== false;
    }}

    function styleParamsForKind(kind) {{
      if (!supportsStyleLayers(kind)) {{
        return {{
          accessory: "none",
          color: colorEl.value,
          expression: "default",
          shape: shapeEl.value,
        }};
      }}
      return {{
        accessory: accessoryEl.value,
        color: colorEl.value,
        expression: expressionEl.value,
        shape: shapeEl.value,
      }};
    }}

    function syncStyleLayerAvailability() {{
      const supportsLayers = supportsStyleLayers(kindEl.value);
      accessoryEl.disabled = !supportsLayers;
      expressionEl.disabled = !supportsLayers;
      if (!supportsLayers) {{
        accessoryEl.value = "none";
        expressionEl.value = "default";
      }}
    }}

    function isPresetIdentity(value) {{
      for (const identity of presetIdentities.values()) {{
        if (value === identity) {{
          return true;
        }}
      }}
      return false;
    }}

    function currentUrl() {{
      const styleParams = styleParamsForKind(kindEl.value);
      const query = new URLSearchParams({{
        id: currentIdentity(),
        tenant: tenantEl.value.trim() || "{tenant}",
        style_version: styleVersionEl.value.trim() || "{style_version}",
        algorithm: "sha512",
        kind: kindEl.value,
        background: backgroundEl.value,
        accessory: styleParams.accessory,
        color: styleParams.color,
        expression: styleParams.expression,
        shape: styleParams.shape,
        format: "webp",
        size: sizeEl.value,
      }});
      return `/v1/avatar?${{query.toString()}}`;
    }}

    function currentSignedUrlEndpoint() {{
      const styleParams = styleParamsForKind(kindEl.value);
      const query = new URLSearchParams({{
        id: currentIdentity(),
        tenant: tenantEl.value.trim() || "{tenant}",
        style_version: styleVersionEl.value.trim() || "{style_version}",
        algorithm: "sha512",
        kind: kindEl.value,
        background: backgroundEl.value,
        accessory: styleParams.accessory,
        color: styleParams.color,
        expression: styleParams.expression,
        shape: styleParams.shape,
        format: "webp",
        size: sizeEl.value,
      }});
      return `/v1/avatar/link?${{query.toString()}}`;
    }}

    async function updateSignedUrl() {{
      if (!storageLinksEnabled) {{
        signedUrlEl.textContent = {signed_unavailable_json};
        copySignedButton.disabled = true;
        return;
      }}

      try {{
        const response = await fetch(currentSignedUrlEndpoint(), {{ headers: {{ "accept": "application/json" }} }});
        if (!response.ok) {{
          signedUrlEl.textContent = {signed_unavailable_json};
          return;
        }}
        const payload = await response.json();
        signedUrlEl.textContent = payload.signed_url;
        copySignedButton.disabled = false;
      }} catch (_) {{
        signedUrlEl.textContent = {signed_unavailable_json};
        copySignedButton.disabled = true;
      }}
    }}

    function refresh() {{
      syncStyleLayerAvailability();
      const url = currentUrl();
      const styleParams = styleParamsForKind(kindEl.value);
      const previewQuery = new URLSearchParams({{
        id: currentIdentity(),
        tenant: tenantEl.value.trim() || "{tenant}",
        style_version: styleVersionEl.value.trim() || "{style_version}",
        algorithm: "sha512",
        kind: kindEl.value,
        background: backgroundEl.value,
        accessory: styleParams.accessory,
        color: styleParams.color,
        expression: styleParams.expression,
        shape: styleParams.shape,
        format: "webp",
        size: sizeEl.value,
        ts: String(Date.now()),
      }});

      previewEl.src = `/v1/avatar?${{previewQuery.toString()}}`;
      urlEl.textContent = `${{window.location.origin}}${{url}}`;
      downloadButton.href = url;
      downloadButton.setAttribute("download", `hashavatar-${{kindEl.value}}.webp`);
      openButton.href = url;
      updateSignedUrl();
      window.hashavatarTelemetry?.avatar({{
        kind: kindEl.value,
        background: backgroundEl.value,
        accessory: styleParams.accessory,
        color: styleParams.color,
        expression: styleParams.expression,
        shape: styleParams.shape,
        size: Number(sizeEl.value) || 0,
      }});
    }}

    function scheduleRefresh() {{
      window.clearTimeout(refreshTimer);
      refreshTimer = window.setTimeout(refresh, 180);
    }}

    function scheduleFullRefresh() {{
      window.clearTimeout(refreshTimer);
      window.clearTimeout(presetRenderTimer);
      refreshTimer = window.setTimeout(refresh, 180);
      presetRenderTimer = window.setTimeout(renderPresetPage, 180);
    }}

    function refreshNowWithPresets() {{
      window.clearTimeout(refreshTimer);
      window.clearTimeout(presetRenderTimer);
      renderPresetPage();
      refresh();
    }}

    function setFromPreset(preset) {{
      identityEl.value = preset.id;
      tenantEl.value = "{tenant}";
      styleVersionEl.value = "{style_version}";
      kindEl.value = preset.kind;
      backgroundEl.value = backgroundEl.value || preset.background;
      sizeEl.value = preset.size;
      refreshNowWithPresets();
    }}

    function renderPresetPage() {{
      const pageCount = Math.ceil(presetExamples.length / presetPageSize);
      presetPage = (presetPage + pageCount) % pageCount;
      const start = presetPage * presetPageSize;
      const pageItems = presetExamples.slice(start, start + presetPageSize);
      const exampleBackground = backgroundEl.value || "themed";
      exampleGrid.replaceChildren();
      for (const preset of pageItems) {{
        const styleParams = styleParamsForKind(preset.kind);
        const button = document.createElement("button");
        button.type = "button";
        button.className = "example-card";
        button.addEventListener("click", () => {{
          window.hashavatarTelemetry?.click("action", "preset-card");
          setFromPreset(preset);
        }});

        const query = new URLSearchParams({{
          id: preset.id,
          tenant: tenantEl.value.trim() || "{tenant}",
          style_version: styleVersionEl.value.trim() || "{style_version}",
          algorithm: "sha512",
          kind: preset.kind,
          background: exampleBackground,
          accessory: styleParams.accessory,
          color: styleParams.color,
          expression: styleParams.expression,
          shape: styleParams.shape,
          format: "webp",
          size: "160",
        }});
        const image = document.createElement("img");
        image.src = `/v1/avatar?${{query.toString()}}`;
        image.alt = `${{preset.label}} {preset_suffix_js}`;

        const title = document.createElement("div");
        title.className = "example-title";
        title.textContent = `${{preset.label}} {preset_suffix_js}`;

        button.append(image, title);
        exampleGrid.append(button);
      }}
      examplePage.textContent = `${{presetPage + 1}} / ${{pageCount}}`;
      presetPrev.disabled = pageCount <= 1;
      presetNext.disabled = pageCount <= 1;
    }}

    async function copyText(text, button, idleText, successText) {{
      try {{
        await navigator.clipboard.writeText(text);
        button.textContent = successText;
      }} catch (_) {{
        button.textContent = {copy_failed_json};
      }}
      window.setTimeout(() => button.textContent = idleText, 1200);
    }}

    copyButton.addEventListener("click", () => {{
      window.hashavatarTelemetry?.click("action", "copy-url");
      copyText(`${{window.location.origin}}${{currentUrl()}}`, copyButton, {copy_url_json}, {copied_json});
    }});
    copySignedButton.addEventListener("click", () => {{
      window.hashavatarTelemetry?.click("action", "copy-signed-link");
      copyText(signedUrlEl.textContent, copySignedButton, {copy_signed_json}, {copied_json});
    }});
    downloadButton.addEventListener("click", () => {{
      window.hashavatarTelemetry?.click("action", "download");
    }});
    openButton.addEventListener("click", () => {{
      window.hashavatarTelemetry?.click("action", "open-raw");
    }});

    [identityEl, tenantEl, styleVersionEl].forEach((el) => {{
      el.addEventListener("input", scheduleFullRefresh);
      el.addEventListener("change", refreshNowWithPresets);
    }});

    sizeEl.addEventListener("input", scheduleRefresh);
    sizeEl.addEventListener("change", refresh);

    [backgroundEl, accessoryEl, colorEl, expressionEl, shapeEl].forEach((el) => {{
      el.addEventListener("input", scheduleRefresh);
      el.addEventListener("change", refreshNowWithPresets);
    }});

    kindEl.addEventListener("change", () => {{
      const current = identityEl.value.trim();
      if (current === "" || isPresetIdentity(current)) {{
        identityEl.value = selectedPresetIdentity();
      }}
      refreshNowWithPresets();
    }});

    presetPrev.addEventListener("click", () => {{
      window.hashavatarTelemetry?.click("action", "preset-prev");
      presetPage -= 1;
      renderPresetPage();
    }});

    presetNext.addEventListener("click", () => {{
      window.hashavatarTelemetry?.click("action", "preset-next");
      presetPage += 1;
      renderPresetPage();
    }});

    renderPresetPage();
    refresh();
  </script>
</body>
</html>"#,
        id = DEFAULT_ID,
        tenant = DEFAULT_NAMESPACE_TENANT,
        style_version = DEFAULT_NAMESPACE_STYLE,
        kind_options = kind_options_html(AvatarKind::Cat),
        background_options = background_options_html(AvatarBackground::Themed),
        accessory_options = accessory_options_html(DEFAULT_ACCESSORY),
        color_options = color_options_html(DEFAULT_COLOR),
        expression_options = expression_options_html(DEFAULT_EXPRESSION),
        shape_options = shape_options_html(DEFAULT_SHAPE),
        preset_examples = preset_examples_json(),
        preset_page_size = PRESET_PAGE_SIZE,
        storage_links_enabled = storage_links_enabled,
        signed_disabled = disabled_attr(!storage_links_enabled),
        meta_tags = render_meta_tags(
            &i18n.t("hero.title", "Public Avatar API"),
            &description,
            "/",
            csp_nonce,
            i18n,
        ),
        styles = shared_page_styles(),
        html_lang = escape_html_attribute(i18n.html_lang()),
        dir = escape_html_attribute(i18n.dir()),
        nav = render_nav_html(i18n),
        hero_eyebrow = i18n.t_attr("hero.eyebrow", "hashavatar.app"),
        hero_title = i18n.t_attr("hero.title", "Generate A Public Avatar In Seconds"),
        hero_description = i18n.t_attr("hero.description", &description),
        hero_privacy_tip = i18n.t_attr("hero.privacy_tip", "Privacy-conscious integration tip: email-shaped identifiers are accepted for convenience, but a stable internal id or one-way hash is better when you want less personal data in URL logs."),
        identity_label = i18n.t_attr("form.identity", "Identity"),
        tenant_label = i18n.t_attr("form.namespace_tenant", "Namespace Tenant"),
        style_version_label = i18n.t_attr("form.style_version", "Style Version"),
        avatar_type_label = i18n.t_attr("form.avatar_type", "Avatar Type"),
        background_label = i18n.t_attr("form.background", "Background"),
        accessory_label = i18n.t_attr("form.accessory", "Accessory"),
        color_label = i18n.t_attr("form.accent_color", "Accent Color"),
        expression_label = i18n.t_attr("form.expression", "Expression"),
        shape_label = i18n.t_attr("form.shape", "Shape"),
        size_label = i18n.t_attr("form.size", "Size"),
        copy_url_label = i18n.t_attr("form.copy_url", "Copy URL"),
        copy_signed_label = i18n.t_attr("form.copy_signed_link", "Copy Signed Link"),
        download_label = i18n.t_attr("form.download", "Download"),
        open_raw_label = i18n.t_attr("form.open_raw", "Open Raw"),
        direct_url_label = i18n.t_attr("form.direct_url", "Direct URL"),
        signed_storage_label = i18n.t_attr("form.signed_storage_link", "Signed Storage Link"),
        signed_unavailable = i18n.t_attr("form.signed_unavailable", "Enable S3 configuration on the server to use signed links."),
        machine_api_label = i18n.t_attr("form.machine_readable_api", "Machine-Readable API"),
        api_label = i18n.t_attr("preview.api", "API"),
        storage_label = i18n.t_attr("preview.storage", "Storage"),
        storage_text = i18n.t_attr("preview.storage_text", "optional S3 persistence with presigned links via"),
        cache_label = i18n.t_attr("preview.cache", "Cache"),
        cache_text = i18n.t_attr("preview.cache_text", "Cloudflare-friendly long cache headers"),
        tip_label = i18n.t_attr("preview.tip", "Tip"),
        tip_text = i18n.t_attr("preview.tip_text", "Every URL is deterministic, so you can embed it directly in your app."),
        preset_examples_label = i18n.t_attr("preview.presets", "Preset Examples"),
        signed_unavailable_json = json_script_string(
            &i18n.t("form.signed_unavailable_runtime", "Signed storage links are unavailable until S3 is configured on the server."),
            "Signed storage links are unavailable until S3 is configured on the server.",
        ),
        preset_suffix_js = i18n.t_attr("preview.preset_suffix", "preset"),
        copy_failed_json = json_script_string(&i18n.t("form.copy_failed", "Copy failed"), "Copy failed"),
        copy_url_json = json_script_string(&i18n.t("form.copy_url", "Copy URL"), "Copy URL"),
        copy_signed_json = json_script_string(
            &i18n.t("form.copy_signed_link", "Copy Signed Link"),
            "Copy Signed Link",
        ),
        copied_json = json_script_string(&i18n.t("form.copied", "Copied"), "Copied"),
        nonce = nonce,
        footer = render_footer_html(i18n, ""),
        telemetry_script = telemetry_script,
    )
}

fn render_help_html(csp_nonce: &CspNonce, telemetry_enabled: bool, i18n: I18n) -> String {
    render_page_html(PageHtmlParams {
        page_title: i18n.t("pages.help.title", "Help"),
        description: i18n.t(
            "pages.help.description",
            "Integration guide for using the hashavatar.app avatar API in web apps, frontends, and backends.",
        ),
        path: "/help",
        eyebrow: i18n.t("pages.help.eyebrow", "Integration Guide"),
        lead: i18n.t(
            "pages.help.lead",
            "Use hashavatar.app directly from the browser, your frontend, or your backend. Every avatar URL is deterministic, so the same identifier and options always produce the same result.",
        ),
        body: format!(
            r#"
<div class="content-grid">
  <section class="card">
    <h2>{basic_url}</h2>
    <p>{basic_url_text}</p>
    <pre><code>https://{site}/v1/avatar?id=robot@hashavatar.app&amp;algorithm=sha512&amp;kind=robot&amp;background=white&amp;accessory=glasses&amp;color=gold&amp;expression=happy&amp;shape=circle&amp;format=webp&amp;size=256</code></pre>
  </section>
  <section class="card">
    <h2>{path_style}</h2>
    <p>{path_style_text}</p>
    <pre><code>https://{site}/avatar/fox/fox@hashavatar.app/webp</code></pre>
  </section>
  <section class="card">
    <h2>{html_example}</h2>
    <pre><code>&lt;img
  src="https://{site}/v1/avatar?id=monster@hashavatar.app&amp;algorithm=sha512&amp;kind=monster&amp;background=themed&amp;accessory=horns&amp;color=crimson&amp;expression=grumpy&amp;shape=hexagon&amp;format=webp&amp;size=256"
  alt="Generated monster avatar"
/&gt;</code></pre>
  </section>
  <section class="card">
    <h2>{javascript_example}</h2>
    <pre><code>const avatarUrl = new URL("https://{site}/v1/avatar");
avatarUrl.search = new URLSearchParams({{
  id: user.email,
  algorithm: "sha512",
  kind: "robot",
  background: "white",
  accessory: "glasses",
  color: "gold",
  expression: "happy",
  shape: "circle",
  format: "webp",
  size: "256",
}}).toString();</code></pre>
  </section>
</div>
<section class="card">
  <h2>{supported_parameters}</h2>
  <ul>
    <li><code>id</code>: any stable identifier such as an email, username, internal user id, or one-way hash</li>
    <li><code>tenant</code>: optional namespace partition for multi-tenant apps</li>
    <li><code>style_version</code>: optional style namespace such as <code>v2</code></li>
    <li><code>algorithm</code>: identity hash mode; only <code>sha512</code> is supported</li>
    <li><code>kind</code>: any public hashavatar family, including <code>cat</code>, <code>dog</code>, <code>robot</code>, <code>planet</code>, <code>rocket</code>, <code>frog</code>, <code>panda</code>, <code>cupcake</code>, <code>pizza</code>, <code>octopus</code>, <code>knight</code>, <code>bear</code>, <code>penguin</code>, <code>dragon</code>, <code>ninja</code>, <code>astronaut</code>, <code>diamond</code>, <code>coffee-cup</code>, and <code>shield</code></li>
    <li><code>background</code>: <code>themed</code>, <code>white</code>, <code>black</code>, <code>dark</code>, <code>light</code>, <code>transparent</code>, <code>polka-dot</code>, <code>striped</code>, <code>checkerboard</code>, <code>grid</code>, <code>sunrise</code>, <code>ocean</code>, or <code>starry</code></li>
    <li><code>accessory</code>: <code>none</code>, <code>glasses</code>, <code>hat</code>, <code>headphones</code>, <code>crown</code>, <code>bowtie</code>, <code>eyepatch</code>, <code>scarf</code>, <code>halo</code>, or <code>horns</code></li>
    <li><code>color</code>: <code>default</code>, <code>neon-mint</code>, <code>pastel-pink</code>, <code>crimson</code>, <code>gold</code>, or <code>deep-sea-blue</code></li>
    <li><code>expression</code>: <code>default</code>, <code>happy</code>, <code>grumpy</code>, <code>surprised</code>, <code>sleepy</code>, <code>winking</code>, <code>cool</code>, or <code>crying</code></li>
    <li><code>shape</code>: <code>square</code>, <code>circle</code>, <code>squircle</code>, <code>hexagon</code>, or <code>octagon</code></li>
    <li><code>format</code>: output format; only <code>webp</code> is supported</li>
    <li><code>size</code>: from <code>64</code> up to <code>1024</code></li>
  </ul>
  <p>{style_layer_note}</p>
</section>
<section class="card">
  <h2>{signed_storage_links}</h2>
  <p>{signed_storage_links_text}</p>
  <pre><code>GET https://{site}/v1/avatar/link?id=robot@hashavatar.app&amp;algorithm=sha512&amp;kind=robot&amp;background=white&amp;accessory=glasses&amp;color=gold&amp;expression=happy&amp;shape=circle&amp;format=webp&amp;size=256</code></pre>
</section>
<section class="card">
  <h2>{open_source}</h2>
  <p>{open_source_text} <a class="inline-link" href="{repo}" target="_blank" rel="noreferrer">{repository}</a> · <a class="inline-link" href="{crate_url}" target="_blank" rel="noreferrer">crates.io</a></p>
</section>
"#,
            site = SITE_NAME,
            repo = REPOSITORY_URL,
            crate_url = CRATE_URL,
            basic_url = i18n.t_attr("pages.help.basic_url", "Basic URL"),
            basic_url_text = i18n.t_attr("pages.help.basic_url_text", "Use the query endpoint when you want a simple public image URL."),
            path_style = i18n.t_attr("pages.help.path_style", "Path Style URL"),
            path_style_text = i18n.t_attr("pages.help.path_style_text", "Use the path form if you prefer cleaner embed URLs."),
            html_example = i18n.t_attr("pages.help.html_example", "HTML Example"),
            javascript_example = i18n.t_attr("pages.help.javascript_example", "JavaScript Example"),
            supported_parameters = i18n.t_attr("pages.help.supported_parameters", "Supported Parameters"),
            style_layer_note = i18n.t_attr("pages.help.style_layer_note", "Accessory and expression layers apply to character-style families."),
            signed_storage_links = i18n.t_attr("pages.help.signed_storage_links", "Signed Storage Links"),
            signed_storage_links_text = i18n.t_attr("pages.help.signed_storage_links_text", "If this deployment has object storage configured, request a presigned storage link from /v1/avatar/link."),
            open_source = i18n.t_attr("pages.help.open_source", "Open Source"),
            open_source_text = i18n.t_attr("pages.help.open_source_text", "The public site source lives in the API repository and the reusable avatar renderer is published on crates.io."),
            repository = i18n.t_attr("nav.repository", "Repository"),
        ),
        csp_nonce,
        telemetry_enabled,
        i18n,
    })
}

fn render_docs_html(csp_nonce: &CspNonce, telemetry_enabled: bool, i18n: I18n) -> String {
    render_page_html(PageHtmlParams {
        page_title: i18n.t("pages.docs.title", "Docs"),
        description: i18n.t("pages.docs.description", "Reference documentation for the hashavatar.app public avatar API, storage link endpoint, and namespace-aware identity contract."),
        path: "/docs",
        eyebrow: i18n.t("pages.docs.eyebrow", "API Reference"),
        lead: i18n.t("pages.docs.lead", "This is the product-facing reference for the public API. The same identity, tenant, style version, avatar family, style options, size, and WebP output are intended to remain stable within a major release."),
        body: format!(
            r#"
<section class="card">
  <h2>{core_endpoints}</h2>
  <ul>
    <li><code>GET /v1/avatar</code>: returns an avatar asset directly</li>
    <li><code>GET /v1/avatar/link</code>: stores the generated avatar in configured object storage and returns signed-link metadata</li>
    <li><code>GET /avatar/&lt;kind&gt;/&lt;identity&gt;/webp</code>: path-style public avatar URL</li>
    <li><code>GET /docs/openapi.json</code>: machine-readable API description</li>
  </ul>
</section>
<section class="card">
  <h2>{operational_endpoints}</h2>
  <p>{operational_text}</p>
</section>
<div class="content-grid">
  <section class="card">
    <h2>{namespace_support}</h2>
    <p>{namespace_text}</p>
    <pre><code>GET https://{site}/v1/avatar?id=wizard@hashavatar.app&amp;tenant=acme&amp;style_version=v2&amp;algorithm=sha512&amp;kind=wizard&amp;background=white&amp;accessory=hat&amp;color=deep-sea-blue&amp;expression=cool&amp;shape=squircle&amp;format=webp&amp;size=256</code></pre>
  </section>
  <section class="card">
    <h2>{anonymous_ids}</h2>
    <p>{anonymous_text}</p>
    <pre><code>printf '%s' 'user@example.com' | sha256sum | cut -d' ' -f1</code></pre>
  </section>
  <section class="card">
    <h2>{rate_limits}</h2>
    <p>{rate_limits_text}</p>
  </section>
  <section class="card">
    <h2>{timeouts}</h2>
    <p>{timeouts_text}</p>
  </section>
</div>
<section class="card">
  <h2>{errors}</h2>
  <ul>
    <li><code>400</code>: {error_bad_request}</li>
    <li><code>408</code>: {error_timeout}</li>
    <li><code>429</code>: {error_rate_limit}</li>
    <li><code>500</code>: {error_internal}</li>
  </ul>
</section>
<section class="card">
  <h2>{openapi}</h2>
  <p>{openapi_text} <a class="inline-link" href="/docs/openapi.json">/docs/openapi.json</a>.</p>
</section>
"#,
            site = SITE_NAME,
            core_endpoints = i18n.t_attr("pages.docs.core_endpoints", "Core Endpoints"),
            operational_endpoints = i18n.t_attr("pages.docs.operational_endpoints", "Operational Endpoints"),
            operational_text = i18n.t_attr("pages.docs.operational_text", "GET /healthz is public for load balancers and uptime checks. GET /metrics is loopback-only and returns 404 to non-local peers."),
            namespace_support = i18n.t_attr("pages.docs.namespace_support", "Namespace Support"),
            namespace_text = i18n.t_attr("pages.docs.namespace_text", "Use tenant and style_version to keep visual identity spaces separate between products or rollout phases."),
            anonymous_ids = i18n.t_attr("pages.docs.anonymous_ids", "Anonymous IDs"),
            anonymous_text = i18n.t_attr("pages.docs.anonymous_text", "Send an internal stable id or a one-way application hash instead of raw personal data."),
            rate_limits = i18n.t_attr("pages.docs.rate_limits", "Rate Limits"),
            rate_limits_text = i18n.t_attr("pages.docs.rate_limits_text", "The public service applies origin-side rate limits."),
            timeouts = i18n.t_attr("pages.docs.timeouts", "Timeouts"),
            timeouts_text = i18n.t_attr("pages.docs.timeouts_text", "Avatar generation and storage operations are bounded by server-side timeouts."),
            errors = i18n.t_attr("pages.docs.errors", "Errors"),
            error_bad_request = i18n.t_attr("pages.docs.error_bad_request", "invalid kind, unsupported algorithm or format, size, or missing identity"),
            error_timeout = i18n.t_attr("pages.docs.error_timeout", "generation or storage timeout"),
            error_rate_limit = i18n.t_attr("pages.docs.error_rate_limit", "rate limit exceeded"),
            error_internal = i18n.t_attr("pages.docs.error_internal", "rendering or storage failure"),
            openapi = i18n.t_attr("pages.docs.openapi", "OpenAPI"),
            openapi_text = i18n.t_attr("pages.docs.openapi_text", "For generated clients or tooling, use"),
        ),
        csp_nonce,
        telemetry_enabled,
        i18n,
    })
}

fn render_terms_html(csp_nonce: &CspNonce, telemetry_enabled: bool, i18n: I18n) -> String {
    render_page_html(PageHtmlParams {
        page_title: i18n.t("pages.terms.title", "Terms"),
        description: i18n.t("pages.terms.description", "Best-effort service terms for the public hashavatar.app avatar API and demo website."),
        path: "/terms",
        eyebrow: i18n.t("pages.terms.eyebrow", "Service Terms"),
        lead: i18n.t("pages.terms.lead", "This public service is provided on an informational and best-effort basis. Use it only if that risk profile works for your application."),
        body: format!(r#"
<section class="card">
  <h2>{no_warranty}</h2>
  <p>{no_warranty_text}</p>
</section>
<section class="card">
  <h2>{no_liability}</h2>
  <p>{no_liability_text}</p>
  <p>{fallback_text}</p>
</section>
<section class="card">
  <h2>{acceptable_use}</h2>
  <p>{acceptable_use_text}</p>
</section>
<section class="card">
  <h2>{changes}</h2>
  <p>{changes_text}</p>
  <p>{legal_note}</p>
</section>
"#,
            no_warranty = i18n.t_attr("pages.terms.no_warranty", "No Warranty"),
            no_warranty_text = i18n.t_attr("pages.terms.no_warranty_text", "This service and all generated outputs are provided as-is and as-available."),
            no_liability = i18n.t_attr("pages.terms.no_liability", "No Liability"),
            no_liability_text = i18n.t_attr("pages.terms.no_liability_text", "We are not responsible for downtime, outages, degraded performance, broken links, cache behavior, lost data, corrupted objects, third-party provider failures, or any direct or indirect damages arising from your use of the service."),
            fallback_text = i18n.t_attr("pages.terms.fallback_text", "If you depend on these avatars in production, you should implement your own fallback behavior, caching strategy, and availability plan."),
            acceptable_use = i18n.t_attr("pages.terms.acceptable_use", "Acceptable Use"),
            acceptable_use_text = i18n.t_attr("pages.terms.acceptable_use_text", "Do not use the service to overload the infrastructure."),
            changes = i18n.t_attr("pages.terms.changes", "Changes"),
            changes_text = i18n.t_attr("pages.terms.changes_text", "We may change, limit, suspend, or discontinue the public service at any time and without notice."),
            legal_note = i18n.t_attr("pages.terms.legal_note", "This page is operational guidance, not legal advice."),
        ),
        csp_nonce,
        telemetry_enabled,
        i18n,
    })
}

fn render_privacy_html(csp_nonce: &CspNonce, telemetry_enabled: bool, i18n: I18n) -> String {
    render_page_html(PageHtmlParams {
        page_title: i18n.t("pages.privacy.title", "Privacy"),
        description: i18n.t("pages.privacy.description", "Privacy notice for hashavatar.app covering request data, logs, and optional object storage behavior."),
        path: "/privacy",
        eyebrow: i18n.t("pages.privacy.eyebrow", "Privacy Notice"),
        lead: i18n.t("pages.privacy.lead", "The service is intentionally simple, but a public avatar API still receives some request data in order to function. This page describes that practical baseline."),
        body: format!(r#"
<section class="card">
  <h2>{receives}</h2>
  <ul>
    <li>{receives_identity}</li>
    <li>{receives_parameters}</li>
    <li>{receives_http_metadata}</li>
  </ul>
</section>
<section class="card">
  <h2>{stores}</h2>
  <p>{stores_text}</p>
  <p>{storage_text}</p>
</section>
<section class="card">
  <h2>{telemetry}</h2>
  <p>{telemetry_text}</p>
  <p>{telemetry_limits}</p>
</section>
<section class="card">
  <h2>{logging}</h2>
  <p>{logging_text}</p>
</section>
<section class="card">
  <h2>{avoid}</h2>
  <p>{avoid_text}</p>
</section>
<section class="card">
  <h2>{repository}</h2>
  <p>{repository_text} <a class="inline-link" href="https://github.com/valkyoth/hashavatar-api" target="_blank" rel="noreferrer">{repo_label}</a> · <a class="inline-link" href="https://crates.io/crates/hashavatar/" target="_blank" rel="noreferrer">{crate_label}</a></p>
</section>
"#,
            receives = i18n.t_attr("pages.privacy.receives", "What The Service Receives"),
            receives_identity = i18n.t_attr("pages.privacy.receives_identity", "the opaque identifier you put in the request, such as an internal id, username, or one-way hash"),
            receives_parameters = i18n.t_attr("pages.privacy.receives_parameters", "request parameters such as avatar type, style options, size, format, and background"),
            receives_http_metadata = i18n.t_attr("pages.privacy.receives_http_metadata", "standard HTTP metadata handled by the server, reverse proxy, and CDN, such as IP address, user agent, referrer, and request timing"),
            stores = i18n.t_attr("pages.privacy.stores", "What The App Itself Stores"),
            stores_text = i18n.t_attr("pages.privacy.stores_text", "The application does not require user accounts and does not set application cookies by default."),
            storage_text = i18n.t_attr("pages.privacy.storage_text", "If object storage support is enabled and a signed-link or persistence route is used, the generated avatar file and its object key may be stored in the configured S3-compatible bucket."),
            telemetry = i18n.t_attr("pages.privacy.telemetry", "Privacy-Preserving Telemetry"),
            telemetry_text = i18n.t_attr("pages.privacy.telemetry_text", "If telemetry is enabled by the operator, the app emits aggregate OpenTelemetry metrics."),
            telemetry_limits = i18n.t_attr("pages.privacy.telemetry_limits", "Telemetry does not include raw identifiers, tenant or style namespace values, IP addresses, user agents, referrers, full URLs, cookies, or free-form text."),
            logging = i18n.t_attr("pages.privacy.logging", "Logging And Infrastructure"),
            logging_text = i18n.t_attr("pages.privacy.logging_text", "Depending on deployment, infrastructure components may keep access logs and operational metadata."),
            avoid = i18n.t_attr("pages.privacy.avoid", "What To Avoid Sending"),
            avoid_text = i18n.t_attr("pages.privacy.avoid_text", "Email-shaped identifiers are accepted for compatibility, but URLs can appear in infrastructure logs."),
            repository = i18n.t_attr("pages.privacy.repository", "Repository And Crate"),
            repository_text = i18n.t_attr("pages.privacy.repository_text", "You can inspect the implementation in the public API repository and the reusable avatar renderer in the Rust crate."),
            repo_label = i18n.t_attr("nav.repository", "Repository"),
            crate_label = i18n.t_attr("nav.rust_crate", "Rust Crate"),
        ),
        csp_nonce,
        telemetry_enabled,
        i18n,
    })
}

#[derive(Debug, Deserialize)]
struct OgQuery {
    id: Option<String>,
    tenant: Option<String>,
    style_version: Option<String>,
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AvatarQuery {
    id: Option<String>,
    kind: Option<String>,
    background: Option<String>,
    accessory: Option<String>,
    color: Option<String>,
    expression: Option<String>,
    shape: Option<String>,
    format: Option<String>,
    algorithm: Option<String>,
    size: Option<u32>,
    tenant: Option<String>,
    style_version: Option<String>,
    persist: Option<bool>,
    redirect: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct PathAvatar {
    kind: String,
    identity: String,
    format: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AvatarRequestFormat {
    Webp,
}

impl AvatarRequestFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Webp => "webp",
        }
    }
}

impl std::fmt::Display for AvatarRequestFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AvatarRequestFormat {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "webp" => Ok(Self::Webp),
            _ => Err(INVALID_AVATAR_FORMAT_MESSAGE),
        }
    }
}

#[derive(Clone)]
struct AvatarRequest {
    identity: String,
    namespace_tenant: String,
    namespace_style: String,
    kind: AvatarKind,
    background: AvatarBackground,
    accessory: AvatarAccessory,
    color: AvatarColor,
    expression: AvatarExpression,
    shape: AvatarShape,
    format: AvatarRequestFormat,
    size: u32,
    persist: bool,
    redirect: bool,
}

impl std::fmt::Debug for AvatarRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AvatarRequest")
            .field("identity", &"[redacted]")
            .field("namespace_tenant", &self.namespace_tenant)
            .field("namespace_style", &self.namespace_style)
            .field("kind", &self.kind)
            .field("background", &self.background)
            .field("accessory", &self.accessory)
            .field("color", &self.color)
            .field("expression", &self.expression)
            .field("shape", &self.shape)
            .field("format", &self.format)
            .field("size", &self.size)
            .field("persist", &self.persist)
            .field("redirect", &self.redirect)
            .finish()
    }
}

impl AvatarRequest {
    fn from_query(query: AvatarQuery) -> Result<Self, String> {
        validate_hash_algorithm(query.algorithm.as_deref())?;
        let format = match query.format.as_deref().map(str::trim) {
            Some(raw) if !raw.is_empty() => AvatarRequestFormat::from_str(raw)
                .map_err(|_| INVALID_AVATAR_FORMAT_MESSAGE.to_string())?,
            _ => AvatarRequestFormat::Webp,
        };

        let request = Self {
            identity: query
                .id
                .map(|value| value.trim().to_string())
                .unwrap_or_else(|| DEFAULT_ID.to_string()),
            namespace_tenant: query
                .tenant
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| DEFAULT_NAMESPACE_TENANT.to_string()),
            namespace_style: query
                .style_version
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| DEFAULT_NAMESPACE_STYLE.to_string()),
            kind: query
                .kind
                .as_deref()
                .and_then(|raw| AvatarKind::from_str(raw).ok())
                .unwrap_or(AvatarKind::Cat),
            background: query
                .background
                .as_deref()
                .and_then(|raw| AvatarBackground::from_str(raw).ok())
                .unwrap_or(AvatarBackground::Themed),
            accessory: query
                .accessory
                .as_deref()
                .and_then(|raw| AvatarAccessory::from_str(raw).ok())
                .unwrap_or(DEFAULT_ACCESSORY),
            color: query
                .color
                .as_deref()
                .and_then(|raw| AvatarColor::from_str(raw).ok())
                .unwrap_or(DEFAULT_COLOR),
            expression: query
                .expression
                .as_deref()
                .and_then(|raw| AvatarExpression::from_str(raw).ok())
                .unwrap_or(DEFAULT_EXPRESSION),
            shape: query
                .shape
                .as_deref()
                .and_then(|raw| AvatarShape::from_str(raw).ok())
                .unwrap_or(DEFAULT_SHAPE),
            format,
            size: query.size.unwrap_or(256),
            persist: query.persist.unwrap_or(false),
            redirect: query.redirect.unwrap_or(false),
        };
        request.validate()?;
        Ok(request)
    }

    fn validate(&self) -> Result<(), String> {
        validate_identity(self.identity.trim())?;
        validate_namespace_component("tenant", &self.namespace_tenant)?;
        validate_namespace_component("style_version", &self.namespace_style)?;
        Ok(())
    }

    fn effective_accessory(&self) -> AvatarAccessory {
        if self.kind.supports_face_layers() {
            self.accessory
        } else {
            DEFAULT_ACCESSORY
        }
    }

    fn effective_expression(&self) -> AvatarExpression {
        if self.kind.supports_face_layers() {
            self.expression
        } else {
            DEFAULT_EXPRESSION
        }
    }

    fn style_options(&self) -> AvatarStyleOptions {
        AvatarStyleOptions::new(
            self.kind,
            self.background,
            self.effective_accessory(),
            self.color,
            self.effective_expression(),
            self.shape,
        )
    }
}

struct AvatarAsset {
    body: Vec<u8>,
    content_type: &'static str,
    cache_key: String,
    object_key: String,
}

#[derive(Serialize)]
struct AvatarLinkResponse {
    object_key: String,
    signed_url: String,
    expires_in_seconds: u64,
    content_type: String,
    cache_key: String,
}

struct SignedStorageObject {
    object_key: String,
    signed_url: String,
}

struct S3Storage {
    client: S3Client,
    bucket: String,
    prefix: String,
    presign_ttl: Duration,
}

impl S3Storage {
    async fn from_env() -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let bucket = match std::env::var("HASHAVATAR_S3_BUCKET") {
            Ok(value) if !value.trim().is_empty() => value,
            _ => return Ok(None),
        };

        let region =
            std::env::var("HASHAVATAR_S3_REGION").unwrap_or_else(|_| "us-east-1".to_string());
        let endpoint = std::env::var("HASHAVATAR_S3_ENDPOINT").ok();
        let force_path_style = std::env::var("HASHAVATAR_S3_PATH_STYLE")
            .ok()
            .map(|raw| matches!(raw.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
            .unwrap_or(false);
        let raw_prefix =
            std::env::var("HASHAVATAR_S3_PREFIX").unwrap_or_else(|_| "avatars".to_string());
        let prefix =
            normalize_s3_prefix(&raw_prefix).map_err(|message| -> Box<dyn std::error::Error> {
                format!("HASHAVATAR_S3_PREFIX: {message}").into()
            })?;
        let requested_ttl = std::env::var("HASHAVATAR_S3_PRESIGN_TTL_SECONDS")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(DEFAULT_S3_PRESIGN_TTL_SECONDS);
        let ttl = clamp_s3_presign_ttl_seconds(requested_ttl);
        if ttl != requested_ttl {
            tracing::warn!(
                requested_ttl_seconds = requested_ttl,
                applied_ttl_seconds = ttl,
                "clamped S3 presign TTL"
            );
        }

        let shared_config = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(region))
            .load()
            .await;

        let mut config_builder = S3ConfigBuilder::from(&shared_config);
        if let Some(endpoint) = endpoint {
            config_builder = config_builder.endpoint_url(endpoint);
        }
        if force_path_style {
            config_builder = config_builder.force_path_style(true);
        }

        Ok(Some(Self {
            client: S3Client::from_conf(config_builder.build()),
            bucket,
            prefix,
            presign_ttl: Duration::from_secs(ttl),
        }))
    }

    async fn store_and_sign(
        &self,
        asset: &AvatarAsset,
        metrics: &Metrics,
    ) -> Result<SignedStorageObject, Box<dyn std::error::Error>> {
        let key = format!("{}/{}", self.prefix, asset.object_key);
        let exists = match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
        {
            Ok(_) => true,
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|error| error.is_not_found()) =>
            {
                false
            }
            Err(error) => return Err(Box::new(error)),
        };

        if exists {
            metrics.storage_hit_total.fetch_add(1, Ordering::Relaxed);
        } else {
            self.client
                .put_object()
                .bucket(&self.bucket)
                .key(&key)
                .body(ByteStream::from(asset.body.clone()))
                .content_type(asset.content_type)
                .cache_control("public, max-age=31536000, immutable")
                .server_side_encryption(ServerSideEncryption::Aes256)
                .send()
                .await?;
            metrics.storage_write_total.fetch_add(1, Ordering::Relaxed);
        }

        let presigned = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&key)
            .presigned(PresigningConfig::expires_in(self.presign_ttl)?)
            .await?;

        Ok(SignedStorageObject {
            object_key: key,
            signed_url: presigned.uri().to_string(),
        })
    }
}

struct HeaderName;

impl HeaderName {
    fn cdn_cache_control() -> axum::http::HeaderName {
        axum::http::HeaderName::from_static("cdn-cache-control")
    }

    fn cloudflare_cache_control() -> axum::http::HeaderName {
        axum::http::HeaderName::from_static("cloudflare-cdn-cache-control")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn response_text(response: Response) -> String {
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .expect("response body");
        std::str::from_utf8(&body).expect("utf8 body").to_string()
    }

    fn test_avatar_request(format: AvatarRequestFormat) -> AvatarRequest {
        AvatarRequest {
            identity: DEFAULT_ID.to_string(),
            namespace_tenant: DEFAULT_NAMESPACE_TENANT.to_string(),
            namespace_style: DEFAULT_NAMESPACE_STYLE.to_string(),
            kind: AvatarKind::Cat,
            background: AvatarBackground::Themed,
            accessory: DEFAULT_ACCESSORY,
            color: DEFAULT_COLOR,
            expression: DEFAULT_EXPRESSION,
            shape: DEFAULT_SHAPE,
            format,
            size: 256,
            persist: false,
            redirect: false,
        }
    }

    #[tokio::test]
    async fn rate_limiter_enforces_per_key_limit() {
        let limiter = RateLimiter::with_capacity(8);
        let key = "avatar:127.0.0.1:public:cat".to_string();

        assert!(limiter.check(key.clone(), 2).await.is_ok());
        assert!(limiter.check(key.clone(), 2).await.is_ok());
        let retry_after = limiter
            .check(key, 2)
            .await
            .expect_err("third request should be rate limited");
        assert!((1..=60).contains(&retry_after));
    }

    #[tokio::test]
    async fn rate_limiter_evicts_oldest_bucket_at_capacity() {
        let limiter = RateLimiter::with_capacity(128);
        let keys = same_rate_limit_shard_keys(&limiter, 3);
        let first = keys[0].clone();
        let second = keys[1].clone();
        let third = keys[2].clone();

        assert!(limiter.check(first.clone(), 1).await.is_ok());
        assert!(limiter.check(second, 1).await.is_ok());
        assert_eq!(limiter.len().await, 2);

        assert!(limiter.check(third, 1).await.is_ok());
        assert_eq!(limiter.len().await, 2);

        assert!(limiter.check(first, 1).await.is_ok());
        assert_eq!(limiter.len().await, 2);
    }

    #[tokio::test]
    async fn rate_limiter_bounds_unique_attacker_keys() {
        let limiter = RateLimiter::with_capacity(32);

        for idx in 0..1_000 {
            assert!(
                limiter
                    .check(format!("avatar:spoofed-{idx}:tenant-{idx}:cat"), 1)
                    .await
                    .is_ok()
            );
        }

        assert_eq!(limiter.len().await, 32);
    }

    #[test]
    fn rate_limiter_capacity_is_churn_resistant() {
        let capacity = MAX_RATE_LIMIT_BUCKETS;
        assert!(capacity >= 65_536);
    }

    #[test]
    fn render_concurrency_has_process_wide_backpressure() {
        assert_eq!(MAX_CONCURRENT_RENDERS, 64);

        let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"));
        let generate_source = source
            .split("async fn generate_avatar_asset(")
            .nth(1)
            .and_then(|after_name| after_name.split("fn build_avatar_asset(").next())
            .expect("generate_avatar_asset source should be present");
        let og_source = source
            .split("async fn og_png(")
            .nth(1)
            .and_then(|after_name| after_name.split("enum OgPngError").next())
            .expect("og_png source should be present");

        assert!(generate_source.contains("try_acquire_owned()"));
        assert!(generate_source.contains("let _render_permit = render_permit;"));
        assert!(og_source.contains("try_acquire_owned()"));
        assert!(og_source.contains("let _render_permit = render_permit;"));
    }

    #[test]
    fn rate_limiter_uses_sharded_hot_path() {
        let limiter = RateLimiter::default();
        assert_eq!(limiter.shards.len(), RATE_LIMIT_SHARDS);
        for shard in limiter.shards.iter() {
            assert_eq!(shard.blocking_lock().buckets.cap().get(), 1_024);
        }
        assert_eq!(
            format!("{:016x}", stable_shard_hash("avatar:8.8.8.8")),
            "f0d0ef909ee80665"
        );
    }

    #[test]
    fn rate_limit_key_is_route_and_ip_scoped() {
        assert_eq!(
            rate_limit_key(RateLimitRoute::Avatar, "203.0.113.10"),
            "avatar:203.0.113.10"
        );
        assert_eq!(
            rate_limit_key(RateLimitRoute::StorageLink, "203.0.113.10"),
            "storage-link:203.0.113.10"
        );
        assert_eq!(
            rate_limit_key(RateLimitRoute::OgImage, "203.0.113.10"),
            "og-image:203.0.113.10"
        );
    }

    #[tokio::test]
    async fn rate_limit_response_includes_retry_after() {
        let state = AppState {
            storage: None,
            trusted_proxies: TrustedProxies::default(),
            rate_limiter: RateLimiter::with_capacity(8),
            metrics: Metrics::default(),
            observability: Observability::disabled(),
        };
        let headers = HeaderMap::new();
        let peer_ip = IpAddr::from([203, 0, 113, 10]);

        for _ in 0..RateLimitRoute::StorageLink.limit() {
            assert!(
                enforce_limits(&state, &headers, peer_ip, RateLimitRoute::StorageLink)
                    .await
                    .is_ok()
            );
        }

        let response = enforce_limits(&state, &headers, peer_ip, RateLimitRoute::StorageLink)
            .await
            .expect_err("request should be rate limited");

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store, max-age=0")
        );
        assert_eq!(
            response
                .headers()
                .get("x-ratelimit-limit")
                .and_then(|value| value.to_str().ok()),
            Some("30")
        );
        assert_eq!(
            response
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|value| value.to_str().ok()),
            Some("0")
        );
        let retry_after = response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .expect("retry-after should be a second count");
        assert!((1..=60).contains(&retry_after));
        assert_eq!(state.metrics.limited_total.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn rate_limiter_uses_non_poisoning_async_mutex() {
        let limiter = RateLimiter::with_capacity(2);
        let shard_index = limiter.shard_index_for("after-poison");
        let shards = limiter.shards.clone();

        let task = tokio::spawn(async move {
            let _guard = shards[shard_index].lock().await;
            panic!("poison rate limiter lock");
        });
        assert!(task.await.expect_err("task should panic").is_panic());

        assert!(limiter.check("after-poison".to_string(), 1).await.is_ok());
    }

    #[test]
    fn s3_presign_ttl_is_bounded_to_sigv4_limits() {
        assert_eq!(clamp_s3_presign_ttl_seconds(1), 60);
        assert_eq!(clamp_s3_presign_ttl_seconds(900), 900);
        assert_eq!(clamp_s3_presign_ttl_seconds(u64::MAX), 604_800);
    }

    #[test]
    fn s3_prefix_is_normalized_and_restricted() {
        assert_eq!(
            normalize_s3_prefix("/avatars/prod/").as_deref(),
            Ok("avatars/prod")
        );
        assert!(normalize_s3_prefix("../secrets").is_err());
        assert!(normalize_s3_prefix("avatars//prod").is_err());
        assert!(normalize_s3_prefix("/").is_err());
    }

    #[test]
    fn s3_head_object_errors_are_not_collapsed_to_cache_miss() {
        let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"));
        let store_source = source
            .split("async fn store_and_sign(")
            .nth(1)
            .and_then(|after_name| after_name.split("let presigned = self").next())
            .expect("store_and_sign source should be present");

        assert!(store_source.contains("as_service_error()"));
        assert!(store_source.contains("is_not_found()"));
        assert!(store_source.contains("server_side_encryption(ServerSideEncryption::Aes256)"));
        assert!(!store_source.contains(".await\n            .is_ok()"));
    }

    fn same_rate_limit_shard_keys(limiter: &RateLimiter, count: usize) -> Vec<String> {
        let mut by_shard = std::collections::BTreeMap::<usize, Vec<String>>::new();
        for idx in 0..10_000 {
            let key = format!("key-{idx}");
            let shard = limiter.shard_index_for(&key);
            let keys = by_shard.entry(shard).or_default();
            keys.push(key);
            if keys.len() == count {
                return keys.clone();
            }
        }

        panic!("failed to find enough keys for one rate-limit shard");
    }

    #[test]
    fn metrics_endpoint_is_loopback_only() {
        assert!(is_loopback_peer(
            "127.0.0.1:8080".parse().expect("ipv4 loopback")
        ));
        assert!(is_loopback_peer(
            "[::1]:8080".parse().expect("ipv6 loopback")
        ));
        assert!(is_loopback_peer(
            "[::ffff:127.0.0.1]:8080"
                .parse()
                .expect("mapped ipv4 loopback")
        ));
        assert!(!is_loopback_peer(
            "198.51.100.10:8080".parse().expect("remote peer")
        ));
    }

    #[test]
    fn otlp_runtime_switch_accepts_enabled_disabled_values() {
        assert!(otlp_enabled_from_vars(|name| {
            (name == "HASHAVATAR_OTLP").then(|| "enabled".to_string())
        }));
        assert!(otlp_enabled_from_vars(|name| {
            (name == "HASHAVATAR_OTEL_ENABLED").then(|| "true".to_string())
        }));
        assert!(!otlp_enabled_from_vars(|name| {
            (name == "HASHAVATAR_OTLP_ENABLED").then(|| "disabled".to_string())
        }));
        assert!(!otlp_enabled_from_vars(|_| None));
    }

    #[test]
    fn visible_page_events_are_allow_listed_and_clamped() {
        let event = Observability::validate_visible_event(VisiblePageEvent {
            route: "/".to_string(),
            section: "home",
            seconds: MAX_VISIBLE_SECONDS + 1,
        })
        .expect("home visible event should be accepted");

        assert_eq!(event.seconds, MAX_VISIBLE_SECONDS);
        assert!(
            Observability::validate_visible_event(VisiblePageEvent {
                route: "/v1/avatar".to_string(),
                section: "avatar",
                seconds: 10,
            })
            .is_none()
        );
        assert_eq!(visible_section_from_str("docs"), Some("docs"));
        assert_eq!(visible_section_from_str("unknown"), None);
        assert!(is_allowed_locale_id("en-EU"));
        assert!(is_allowed_locale_id("de-DE"));
        assert!(is_allowed_locale_id("es-ES"));
        assert!(is_allowed_locale_id("it-IT"));
        assert!(is_allowed_locale_id("pl-PL"));
        assert!(is_allowed_locale_id("cs-CZ"));
        assert!(is_allowed_locale_id("sk-SK"));
        assert!(is_allowed_locale_id("sq-AL"));
        assert!(is_allowed_locale_id("bs-BA"));
        assert!(is_allowed_locale_id("mk-MK"));
        assert!(is_allowed_locale_id("mt-MT"));
        assert!(is_allowed_locale_id("hy-AM"));
        assert!(is_allowed_locale_id("ka-GE"));
        assert!(is_allowed_locale_id("az-AZ"));
        assert!(is_allowed_locale_id("kk-KZ"));
        assert!(is_allowed_locale_id("uz-UZ"));
        assert!(is_allowed_locale_id("ky-KG"));
        assert!(is_allowed_locale_id("tg-TJ"));
        assert!(is_allowed_locale_id("tk-TM"));
        assert!(is_allowed_locale_id("ps-AF"));
        assert!(is_allowed_locale_id("ckb-IQ"));
        assert!(is_allowed_locale_id("ku-TR"));
        assert!(is_allowed_locale_id("ti-ER"));
        assert!(is_allowed_locale_id("rw-RW"));
        assert!(is_allowed_locale_id("mg-MG"));
        assert!(is_allowed_locale_id("sn-ZW"));
        assert!(is_allowed_locale_id("xh-ZA"));
        assert!(!is_allowed_locale_id("nn-NO"));
    }

    #[test]
    fn telemetry_events_use_bounded_allow_listed_labels() {
        assert!(is_allowed_click("github", "repository"));
        assert!(is_allowed_click("outbound", "crate"));
        assert!(is_allowed_click("action", "copy-url"));
        assert!(!is_allowed_click(
            "github",
            "https://github.com/valkyoth/hashavatar-api"
        ));
        assert!(!is_allowed_click("action", "custom-user-input"));

        let style = avatar_telemetry_style_from_payload(&AvatarGenerateTelemetryPayload {
            locale: "en-EU".to_string(),
            kind: "paws".to_string(),
            background: "themed".to_string(),
            accessory: "glasses".to_string(),
            color: "gold".to_string(),
            expression: "happy".to_string(),
            shape: "circle".to_string(),
            size: 256,
        })
        .expect("bounded style telemetry payload should parse");

        assert_eq!(style.kind, AvatarKind::Paws);
        assert_eq!(style.accessory, DEFAULT_ACCESSORY);
        assert_eq!(style.expression, DEFAULT_EXPRESSION);
        assert_eq!(style.size_bucket, "256-511");
        assert!(
            avatar_telemetry_style_from_payload(&AvatarGenerateTelemetryPayload {
                locale: "en-EU".to_string(),
                kind: "<script>".to_string(),
                background: "themed".to_string(),
                accessory: "none".to_string(),
                color: "default".to_string(),
                expression: "default".to_string(),
                shape: "square".to_string(),
                size: 256,
            })
            .is_err()
        );
    }

    #[tokio::test]
    async fn healthz_only_exposes_liveness() {
        let response = healthz().await.into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_text(response).await;
        let payload: serde_json::Value =
            serde_json::from_str(&body).expect("healthz should return json");

        assert_eq!(payload, serde_json::json!({"status": "ok"}));
        assert!(payload.get("service").is_none());
        assert!(payload.get("s3_enabled").is_none());
        assert!(payload.get("style_version").is_none());
    }

    #[test]
    fn metrics_generation_duration_saturates_at_u64_max() {
        let metrics = Metrics::default();

        metrics
            .generation_millis_total
            .store(u64::MAX - 10, Ordering::Relaxed);
        metrics.observe_generation(Duration::from_millis(25));

        assert_eq!(
            metrics.generation_millis_total.load(Ordering::Relaxed),
            u64::MAX
        );

        metrics.observe_generation(Duration::from_secs(u64::MAX));

        assert_eq!(
            metrics.generation_millis_total.load(Ordering::Relaxed),
            u64::MAX
        );
    }

    #[test]
    fn content_security_policy_uses_nonce_without_unsafe_inline() {
        let nonce = CspNonce("testnonce".to_string());
        let policy = content_security_policy(&nonce);

        assert!(policy.contains("style-src 'self' 'nonce-testnonce'"));
        assert!(policy.contains("script-src 'self' 'nonce-testnonce'"));
        assert!(!policy.contains("unsafe-inline"));
    }

    #[test]
    fn non_html_routes_use_static_csp_without_nonce() {
        assert!(route_uses_inline_html("/"));
        assert!(route_uses_inline_html("/docs"));
        assert!(route_uses_inline_html("/de/docs"));
        assert!(route_uses_inline_html("/fr/privacy"));
        assert!(!route_uses_inline_html("/v1/avatar"));
        assert!(!route_uses_inline_html("/og.png"));
        assert!(!static_content_security_policy().contains("nonce-"));
        assert!(static_content_security_policy().contains("script-src 'self'"));
    }

    #[test]
    fn locale_config_loads_requested_languages() {
        assert_eq!(default_locale().locale_id, "en-EU");
        assert_eq!(locales().len(), 112);
        assert_eq!(locale_by_prefix("en-gb").unwrap().locale_id, "en-GB");
        assert_eq!(locale_by_prefix("en-us").unwrap().locale_id, "en-US");
        assert_eq!(locale_by_prefix("fr").unwrap().display_name, "Français");
        assert_eq!(locale_by_prefix("de").unwrap().display_name, "Deutsch");
        assert_eq!(locale_by_prefix("sv").unwrap().display_name, "Svenska");
        assert_eq!(locale_by_prefix("no").unwrap().display_name, "Norsk");
        assert_eq!(locale_by_prefix("nl").unwrap().display_name, "Nederlands");
        assert_eq!(locale_by_prefix("fi").unwrap().display_name, "Suomi");
        assert_eq!(locale_by_prefix("is").unwrap().display_name, "Íslenska");
        assert_eq!(locale_by_prefix("es").unwrap().display_name, "Español");
        assert_eq!(locale_by_prefix("pt").unwrap().display_name, "Português");
        assert_eq!(locale_by_prefix("it").unwrap().display_name, "Italiano");
        assert_eq!(locale_by_prefix("ja").unwrap().display_name, "日本語");
        assert_eq!(locale_by_prefix("zh").unwrap().display_name, "简体中文");
        assert_eq!(locale_by_prefix("zh-tw").unwrap().display_name, "繁體中文");
        assert_eq!(locale_by_prefix("vi").unwrap().display_name, "Tiếng Việt");
        assert_eq!(locale_by_prefix("th").unwrap().display_name, "ไทย");
        assert_eq!(locale_by_prefix("hi").unwrap().display_name, "हिन्दी");
        assert_eq!(locale_by_prefix("bn").unwrap().display_name, "বাংলা");
        assert_eq!(locale_by_prefix("ta").unwrap().display_name, "தமிழ்");
        assert_eq!(locale_by_prefix("eo").unwrap().display_name, "Esperanto");
        assert_eq!(locale_by_prefix("da").unwrap().display_name, "Dansk");
        assert_eq!(locale_by_prefix("la").unwrap().display_name, "Latina");
        assert_eq!(
            locale_by_prefix("gsw").unwrap().display_name,
            "Schwiizerdütsch"
        );
        assert_eq!(locale_by_prefix("ko").unwrap().display_name, "한국어");
        assert_eq!(locale_by_prefix("ru").unwrap().display_name, "Русский");
        assert_eq!(locale_by_prefix("uk").unwrap().display_name, "Українська");
        assert_eq!(locale_by_prefix("vlaams").unwrap().display_name, "Vlaams");
        assert_eq!(
            locale_by_prefix("fr-be").unwrap().display_name,
            "Français (Belgique)"
        );
        assert_eq!(
            locale_by_prefix("fr-ca").unwrap().display_name,
            "Français (Canada)"
        );
        assert_eq!(
            locale_by_prefix("en-ca").unwrap().display_name,
            "English (Canada)"
        );
        assert_eq!(locale_by_prefix("tr").unwrap().display_name, "Türkçe");
        assert_eq!(locale_by_prefix("lt").unwrap().display_name, "Lietuvių");
        assert_eq!(locale_by_prefix("lv").unwrap().display_name, "Latviešu");
        assert_eq!(locale_by_prefix("pl").unwrap().display_name, "Polski");
        assert_eq!(locale_by_prefix("el").unwrap().display_name, "Ελληνικά");
        assert_eq!(locale_by_prefix("hu").unwrap().display_name, "Magyar");
        assert_eq!(locale_by_prefix("et").unwrap().display_name, "Eesti");
        assert_eq!(locale_by_prefix("ovd").unwrap().display_name, "Övdalsk");
        assert_eq!(locale_by_prefix("bg").unwrap().display_name, "Български");
        assert_eq!(locale_by_prefix("cs").unwrap().display_name, "Čeština");
        assert_eq!(locale_by_prefix("hr").unwrap().display_name, "Hrvatski");
        assert_eq!(locale_by_prefix("be").unwrap().display_name, "Беларуская");
        assert_eq!(locale_by_prefix("ga").unwrap().display_name, "Gaeilge");
        assert_eq!(
            locale_by_prefix("lb").unwrap().display_name,
            "Lëtzebuergesch"
        );
        assert_eq!(locale_by_prefix("ro").unwrap().display_name, "Română");
        assert_eq!(locale_by_prefix("sr").unwrap().display_name, "Српски");
        assert_eq!(locale_by_prefix("nap").unwrap().display_name, "Napulitano");
        assert_eq!(locale_by_prefix("sk").unwrap().display_name, "Slovenčina");
        assert_eq!(locale_by_prefix("sl").unwrap().display_name, "Slovenščina");
        assert_eq!(locale_by_prefix("fy").unwrap().display_name, "Frysk");
        assert_eq!(
            locale_by_prefix("se").unwrap().display_name,
            "Davvisámegiella"
        );
        assert_eq!(locale_by_prefix("scn").unwrap().display_name, "Sicilianu");
        assert_eq!(
            locale_by_prefix("ar").unwrap().display_name,
            "العربية الفصحى"
        );
        assert_eq!(locale_by_prefix("ar").unwrap().dir.as_deref(), Some("rtl"));
        assert_eq!(
            locale_by_prefix("ar-ae").unwrap().display_name,
            "العربية الإماراتية"
        );
        assert_eq!(
            locale_by_prefix("ar-eg").unwrap().display_name,
            "العربية المصرية"
        );
        assert_eq!(
            locale_by_prefix("ar-sa").unwrap().display_name,
            "العربية السعودية"
        );
        assert_eq!(
            locale_by_prefix("id").unwrap().display_name,
            "Bahasa Indonesia"
        );
        assert_eq!(locale_by_prefix("ur").unwrap().display_name, "اردو");
        assert_eq!(locale_by_prefix("ur").unwrap().dir.as_deref(), Some("rtl"));
        assert_eq!(locale_by_prefix("mr").unwrap().display_name, "मराठी");
        assert_eq!(locale_by_prefix("jv").unwrap().display_name, "Basa Jawa");
        assert_eq!(
            locale_by_prefix("pt-br").unwrap().display_name,
            "Português (Brasil)"
        );
        assert_eq!(
            locale_by_prefix("es-mx").unwrap().display_name,
            "Español (México)"
        );
        assert_eq!(
            locale_by_prefix("ms").unwrap().display_name,
            "Bahasa Melayu"
        );
        assert_eq!(locale_by_prefix("fil").unwrap().display_name, "Filipino");
        assert_eq!(locale_by_prefix("fa").unwrap().display_name, "فارسی");
        assert_eq!(locale_by_prefix("fa").unwrap().dir.as_deref(), Some("rtl"));
        assert_eq!(locale_by_prefix("he").unwrap().display_name, "עברית");
        assert_eq!(locale_by_prefix("he").unwrap().dir.as_deref(), Some("rtl"));
        assert_eq!(locale_by_prefix("sw").unwrap().display_name, "Kiswahili");
        assert_eq!(locale_by_prefix("pa").unwrap().display_name, "ਪੰਜਾਬੀ");
        assert_eq!(locale_by_prefix("te").unwrap().display_name, "తెలుగు");
        assert_eq!(locale_by_prefix("gu").unwrap().display_name, "ગુજરાતી");
        assert_eq!(locale_by_prefix("kn").unwrap().display_name, "ಕನ್ನಡ");
        assert_eq!(locale_by_prefix("ml").unwrap().display_name, "മലയാളം");
        assert_eq!(locale_by_prefix("ne").unwrap().display_name, "नेपाली");
        assert_eq!(locale_by_prefix("si").unwrap().display_name, "සිංහල");
        assert_eq!(locale_by_prefix("my").unwrap().display_name, "မြန်မာ");
        assert_eq!(locale_by_prefix("km").unwrap().display_name, "ភាសាខ្មែរ");
        assert_eq!(locale_by_prefix("lo").unwrap().display_name, "ລາວ");
        assert_eq!(locale_by_prefix("mn").unwrap().display_name, "Монгол");
        assert_eq!(locale_by_prefix("ha").unwrap().display_name, "Hausa");
        assert_eq!(locale_by_prefix("yo").unwrap().display_name, "Yorùbá");
        assert_eq!(locale_by_prefix("ig").unwrap().display_name, "Igbo");
        assert_eq!(locale_by_prefix("am").unwrap().display_name, "አማርኛ");
        assert_eq!(locale_by_prefix("om").unwrap().display_name, "Afaan Oromoo");
        assert_eq!(locale_by_prefix("so").unwrap().display_name, "Soomaali");
        assert_eq!(locale_by_prefix("zu").unwrap().display_name, "isiZulu");
        assert_eq!(locale_by_prefix("af").unwrap().display_name, "Afrikaans");
        assert_eq!(locale_by_prefix("ca").unwrap().display_name, "Català");
        assert_eq!(locale_by_prefix("eu").unwrap().display_name, "Euskara");
        assert_eq!(locale_by_prefix("gl").unwrap().display_name, "Galego");
        assert_eq!(locale_by_prefix("cy").unwrap().display_name, "Cymraeg");
        assert_eq!(locale_by_prefix("sq").unwrap().display_name, "Shqip");
        assert_eq!(locale_by_prefix("bs").unwrap().display_name, "Bosanski");
        assert_eq!(locale_by_prefix("mk").unwrap().display_name, "Македонски");
        assert_eq!(locale_by_prefix("mt").unwrap().display_name, "Malti");
        assert_eq!(locale_by_prefix("hy").unwrap().display_name, "Հայերեն");
        assert_eq!(locale_by_prefix("ka").unwrap().display_name, "ქართული");
        assert_eq!(locale_by_prefix("az").unwrap().display_name, "Azərbaycanca");
        assert_eq!(locale_by_prefix("kk").unwrap().display_name, "Қазақша");
        assert_eq!(locale_by_prefix("uz").unwrap().display_name, "Oʻzbekcha");
        assert_eq!(locale_by_prefix("ky").unwrap().display_name, "Кыргызча");
        assert_eq!(locale_by_prefix("tg").unwrap().display_name, "Тоҷикӣ");
        assert_eq!(locale_by_prefix("tk").unwrap().display_name, "Türkmençe");
        assert_eq!(locale_by_prefix("ps").unwrap().display_name, "پښتو");
        assert_eq!(locale_by_prefix("ps").unwrap().dir.as_deref(), Some("rtl"));
        assert_eq!(
            locale_by_prefix("ckb").unwrap().display_name,
            "کوردیی ناوەندی"
        );
        assert_eq!(locale_by_prefix("ckb").unwrap().dir.as_deref(), Some("rtl"));
        assert_eq!(locale_by_prefix("ku").unwrap().display_name, "Kurdî");
        assert_eq!(locale_by_prefix("ti").unwrap().display_name, "ትግርኛ");
        assert_eq!(locale_by_prefix("rw").unwrap().display_name, "Kinyarwanda");
        assert_eq!(locale_by_prefix("mg").unwrap().display_name, "Malagasy");
        assert_eq!(locale_by_prefix("sn").unwrap().display_name, "chiShona");
        assert_eq!(locale_by_prefix("xh").unwrap().display_name, "isiXhosa");
        assert!(locale_by_prefix("nn").is_none());
    }

    #[test]
    fn security_headers_include_modern_isolation_policy() {
        let mut html_response = Html("ok").into_response();
        apply_security_headers(
            html_response.headers_mut(),
            &content_security_policy(&CspNonce("testnonce".to_string())),
            true,
        );

        assert_eq!(
            html_response
                .headers()
                .get("cross-origin-resource-policy")
                .and_then(|value| value.to_str().ok()),
            Some("cross-origin")
        );
        assert_eq!(
            html_response
                .headers()
                .get("cross-origin-opener-policy")
                .and_then(|value| value.to_str().ok()),
            Some("same-origin")
        );
        assert_eq!(
            html_response
                .headers()
                .get("strict-transport-security")
                .and_then(|value| value.to_str().ok()),
            Some("max-age=31536000; includeSubDomains")
        );
        let permissions_policy = html_response
            .headers()
            .get("permissions-policy")
            .and_then(|value| value.to_str().ok())
            .expect("permissions policy header");
        assert!(permissions_policy.contains("usb=()"));
        assert!(permissions_policy.contains("clipboard-read=()"));
        assert_eq!(
            html_response
                .headers()
                .get("x-permitted-cross-domain-policies")
                .and_then(|value| value.to_str().ok()),
            Some("none")
        );

        let mut image_headers = cache_headers("\"etag\"");
        apply_security_headers(
            &mut image_headers,
            &content_security_policy(&CspNonce("testnonce".to_string())),
            false,
        );

        assert_eq!(
            image_headers
                .get("cross-origin-resource-policy")
                .and_then(|value| value.to_str().ok()),
            Some("cross-origin")
        );
        assert!(!image_headers.contains_key("cross-origin-opener-policy"));
        assert_eq!(
            image_headers
                .get("strict-transport-security")
                .and_then(|value| value.to_str().ok()),
            Some("max-age=31536000; includeSubDomains")
        );
    }

    #[test]
    fn error_responses_are_not_cacheable() {
        for response in [
            bad_request("bad"),
            internal_error("detail"),
            request_timeout("timeout"),
            server_busy(),
        ] {
            assert_eq!(
                response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some("no-store, max-age=0")
            );
        }
    }

    #[test]
    fn standard_avatar_response_does_not_emit_signed_storage_headers() {
        let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"));
        let serve_avatar_source = source
            .split("async fn serve_avatar(")
            .nth(1)
            .and_then(|after_name| after_name.split("async fn serve_avatar_link(").next())
            .expect("serve_avatar source should be present");
        let serve_avatar_link_source = source
            .split("async fn serve_avatar_link(")
            .nth(1)
            .and_then(|after_name| after_name.split("async fn generate_avatar_asset(").next())
            .expect("serve_avatar_link source should be present");

        assert!(!serve_avatar_source.contains("HeaderName::storage_key()"));
        assert!(!serve_avatar_source.contains("HeaderName::signed_url()"));
        assert!(source.contains("async fn serve_avatar_link("));
        assert!(serve_avatar_link_source.contains("object_key: signed.object_key"));
        assert!(serve_avatar_link_source.contains("signed_url: signed.signed_url"));
        assert!(serve_avatar_link_source.contains("cache_key: sha256_hex(&asset.cache_key)"));
        assert!(!serve_avatar_link_source.contains("cache_key: asset.cache_key"));
    }

    #[test]
    fn rendered_index_applies_csp_nonce_to_inline_blocks() {
        let nonce = CspNonce("testnonce".to_string());
        let html = render_index_html(&nonce, false, true, i18n(default_locale()));

        assert!(html.contains(r#"<style nonce="testnonce">"#));
        assert!(html.contains(r#"<script nonce="testnonce">"#));
        assert!(html.contains(r#"<script nonce="testnonce" type="application/ld+json">"#));
        assert!(html.contains("window.hashavatarTelemetry"));
        assert!(html.contains("/telemetry/page-visible"));
        assert!(html.contains("/telemetry/avatar-generate"));
        assert!(html.contains("window.hashavatarTelemetry?.avatar({"));
        assert!(html.contains(r#"kind: kindEl.value"#));
        assert!(!html.contains("window.hashavatarTelemetry?.avatar({\n        id:"));
    }

    #[test]
    fn rendered_index_omits_telemetry_script_when_disabled() {
        let nonce = CspNonce("testnonce".to_string());
        let html = render_index_html(&nonce, false, false, i18n(default_locale()));

        assert!(!html.contains("window.hashavatarTelemetry ="));
        assert!(!html.contains("/telemetry/page-visible"));
        assert!(!html.contains("/telemetry/avatar-generate"));
    }

    #[test]
    fn language_switcher_links_same_page_without_touching_avatar_urls() {
        let nonce = CspNonce("testnonce".to_string());
        let german = i18n(locale_by_id("de-DE").expect("German locale"));
        let html = render_index_html(&nonce, false, false, german);

        assert!(html.contains(r#"<html lang="de-DE" dir="ltr">"#));
        assert!(html.contains("Öffentliche Avatare in Sekunden erzeugen"));
        assert!(html.contains(r#"<details class="language-switcher">"#));
        assert!(html.contains(r#"<a href="/">🇪🇺 English (EU)</a>"#));
        assert!(html.contains(r#"<a href="/en-gb/">🇬🇧 English (UK)</a>"#));
        assert!(html.contains(r#"<a href="/en-us/">🇺🇸 English (US)</a>"#));
        assert!(html.contains(r#"<a href="/fr/">🇫🇷 Français</a>"#));
        assert!(html.contains(r#"<a href="/de/" aria-current="true">🇩🇪 Deutsch</a>"#));
        assert!(html.contains(r#"<a href="/sv/">🇸🇪 Svenska</a>"#));
        assert!(html.contains(r#"<a href="/no/">🇳🇴 Norsk</a>"#));
        assert!(html.contains(r#"<a href="/nl/">🇳🇱 Nederlands</a>"#));
        assert!(html.contains(r#"<a href="/fi/">🇫🇮 Suomi</a>"#));
        assert!(html.contains(r#"<a href="/is/">🇮🇸 Íslenska</a>"#));
        assert!(html.contains(r#"<a href="/es/">🇪🇸 Español</a>"#));
        assert!(html.contains(r#"<a href="/pt/">🇵🇹 Português</a>"#));
        assert!(html.contains(r#"<a href="/it/">🇮🇹 Italiano</a>"#));
        assert!(html.contains(r#"<a href="/ja/">🇯🇵 日本語</a>"#));
        assert!(html.contains(r#"<a href="/zh/">🇨🇳 简体中文</a>"#));
        assert!(html.contains(r#"<a href="/zh-tw/">🇹🇼 繁體中文</a>"#));
        assert!(html.contains(r#"<a href="/vi/">🇻🇳 Tiếng Việt</a>"#));
        assert!(html.contains(r#"<a href="/th/">🇹🇭 ไทย</a>"#));
        assert!(html.contains(r#"<a href="/hi/">🇮🇳 हिन्दी</a>"#));
        assert!(html.contains(r#"<a href="/bn/">🇮🇳 বাংলা</a>"#));
        assert!(html.contains(r#"<a href="/ta/">🇮🇳 தமிழ்</a>"#));
        assert!(html.contains(r#"<a href="/eo/">🌐 Esperanto</a>"#));
        assert!(html.contains(r#"<a href="/da/">🇩🇰 Dansk</a>"#));
        assert!(html.contains(r#"<a href="/la/">🇻🇦 Latina</a>"#));
        assert!(html.contains(r#"<a href="/gsw/">🇨🇭 Schwiizerdütsch</a>"#));
        assert!(html.contains(r#"<a href="/ko/">🇰🇷 한국어</a>"#));
        assert!(html.contains(r#"<a href="/ru/">🇷🇺 Русский</a>"#));
        assert!(html.contains(r#"<a href="/uk/">🇺🇦 Українська</a>"#));
        assert!(html.contains(r#"<a href="/vlaams/">🇧🇪 Vlaams</a>"#));
        assert!(html.contains(r#"<a href="/fr-be/">🇧🇪 Français (Belgique)</a>"#));
        assert!(html.contains(r#"<a href="/fr-ca/">🇨🇦 Français (Canada)</a>"#));
        assert!(html.contains(r#"<a href="/en-ca/">🇨🇦 English (Canada)</a>"#));
        assert!(html.contains(r#"<a href="/tr/">🇹🇷 Türkçe</a>"#));
        assert!(html.contains(r#"<a href="/lt/">🇱🇹 Lietuvių</a>"#));
        assert!(html.contains(r#"<a href="/lv/">🇱🇻 Latviešu</a>"#));
        assert!(html.contains(r#"<a href="/pl/">🇵🇱 Polski</a>"#));
        assert!(html.contains(r#"<a href="/el/">🇬🇷 Ελληνικά</a>"#));
        assert!(html.contains(r#"<a href="/hu/">🇭🇺 Magyar</a>"#));
        assert!(html.contains(r#"<a href="/et/">🇪🇪 Eesti</a>"#));
        assert!(html.contains(r#"<a href="/ovd/">🇸🇪 Övdalsk</a>"#));
        assert!(html.contains(r#"<a href="/bg/">🇧🇬 Български</a>"#));
        assert!(html.contains(r#"<a href="/cs/">🇨🇿 Čeština</a>"#));
        assert!(html.contains(r#"<a href="/hr/">🇭🇷 Hrvatski</a>"#));
        assert!(html.contains(r#"<a href="/be/">🇧🇾 Беларуская</a>"#));
        assert!(html.contains(r#"<a href="/ga/">🇮🇪 Gaeilge</a>"#));
        assert!(html.contains(r#"<a href="/lb/">🇱🇺 Lëtzebuergesch</a>"#));
        assert!(html.contains(r#"<a href="/ro/">🇷🇴 Română</a>"#));
        assert!(html.contains(r#"<a href="/sr/">🇷🇸 Српски</a>"#));
        assert!(html.contains(r#"<a href="/nap/">🇮🇹 Napulitano</a>"#));
        assert!(html.contains(r#"<a href="/sk/">🇸🇰 Slovenčina</a>"#));
        assert!(html.contains(r#"<a href="/sl/">🇸🇮 Slovenščina</a>"#));
        assert!(html.contains(r#"<a href="/fy/">🇳🇱 Frysk</a>"#));
        assert!(html.contains(r#"<a href="/se/">🇳🇴 Davvisámegiella</a>"#));
        assert!(html.contains(r#"<a href="/scn/">🇮🇹 Sicilianu</a>"#));
        assert!(html.contains(r#"<a href="/ar/">🌐 العربية الفصحى</a>"#));
        assert!(html.contains(r#"<a href="/ar-ae/">🇦🇪 العربية الإماراتية</a>"#));
        assert!(html.contains(r#"<a href="/ar-eg/">🇪🇬 العربية المصرية</a>"#));
        assert!(html.contains(r#"<a href="/ar-sa/">🇸🇦 العربية السعودية</a>"#));
        assert!(html.contains(r#"<a href="/id/">🇮🇩 Bahasa Indonesia</a>"#));
        assert!(html.contains(r#"<a href="/ur/">🇵🇰 اردو</a>"#));
        assert!(html.contains(r#"<a href="/mr/">🇮🇳 मराठी</a>"#));
        assert!(html.contains(r#"<a href="/jv/">🇮🇩 Basa Jawa</a>"#));
        assert!(html.contains(r#"<a href="/pt-br/">🇧🇷 Português (Brasil)</a>"#));
        assert!(html.contains(r#"<a href="/es-mx/">🇲🇽 Español (México)</a>"#));
        assert!(html.contains(r#"<a href="/ms/">🇲🇾 Bahasa Melayu</a>"#));
        assert!(html.contains(r#"<a href="/fil/">🇵🇭 Filipino</a>"#));
        assert!(html.contains(r#"<a href="/fa/">🇮🇷 فارسی</a>"#));
        assert!(html.contains(r#"<a href="/he/">🇮🇱 עברית</a>"#));
        assert!(html.contains(r#"<a href="/sw/">🇰🇪 Kiswahili</a>"#));
        assert!(html.contains(r#"<a href="/pa/">🇮🇳 ਪੰਜਾਬੀ</a>"#));
        assert!(html.contains(r#"<a href="/te/">🇮🇳 తెలుగు</a>"#));
        assert!(html.contains(r#"<a href="/gu/">🇮🇳 ગુજરાતી</a>"#));
        assert!(html.contains(r#"<a href="/kn/">🇮🇳 ಕನ್ನಡ</a>"#));
        assert!(html.contains(r#"<a href="/ml/">🇮🇳 മലയാളം</a>"#));
        assert!(html.contains(r#"<a href="/ne/">🇳🇵 नेपाली</a>"#));
        assert!(html.contains(r#"<a href="/si/">🇱🇰 සිංහල</a>"#));
        assert!(html.contains(r#"<a href="/my/">🇲🇲 မြန်မာ</a>"#));
        assert!(html.contains(r#"<a href="/km/">🇰🇭 ភាសាខ្មែរ</a>"#));
        assert!(html.contains(r#"<a href="/lo/">🇱🇦 ລາວ</a>"#));
        assert!(html.contains(r#"<a href="/mn/">🇲🇳 Монгол</a>"#));
        assert!(html.contains(r#"<a href="/ha/">🇳🇬 Hausa</a>"#));
        assert!(html.contains(r#"<a href="/yo/">🇳🇬 Yorùbá</a>"#));
        assert!(html.contains(r#"<a href="/ig/">🇳🇬 Igbo</a>"#));
        assert!(html.contains(r#"<a href="/am/">🇪🇹 አማርኛ</a>"#));
        assert!(html.contains(r#"<a href="/om/">🇪🇹 Afaan Oromoo</a>"#));
        assert!(html.contains(r#"<a href="/so/">🇸🇴 Soomaali</a>"#));
        assert!(html.contains(r#"<a href="/zu/">🇿🇦 isiZulu</a>"#));
        assert!(html.contains(r#"<a href="/af/">🇿🇦 Afrikaans</a>"#));
        assert!(html.contains(r#"<a href="/ca/">🇪🇸 Català</a>"#));
        assert!(html.contains(r#"<a href="/eu/">🇪🇸 Euskara</a>"#));
        assert!(html.contains(r#"<a href="/gl/">🇪🇸 Galego</a>"#));
        assert!(html.contains(r#"<a href="/cy/">🏴 Cymraeg</a>"#));
        assert!(html.contains(r#"<a href="/sq/">🇦🇱 Shqip</a>"#));
        assert!(html.contains(r#"<a href="/bs/">🇧🇦 Bosanski</a>"#));
        assert!(html.contains(r#"<a href="/mk/">🇲🇰 Македонски</a>"#));
        assert!(html.contains(r#"<a href="/mt/">🇲🇹 Malti</a>"#));
        assert!(html.contains(r#"<a href="/hy/">🇦🇲 Հայերեն</a>"#));
        assert!(html.contains(r#"<a href="/ka/">🇬🇪 ქართული</a>"#));
        assert!(html.contains(r#"<a href="/az/">🇦🇿 Azərbaycanca</a>"#));
        assert!(html.contains(r#"<a href="/kk/">🇰🇿 Қазақша</a>"#));
        assert!(html.contains(r#"<a href="/uz/">🇺🇿 Oʻzbekcha</a>"#));
        assert!(html.contains(r#"<a href="/ky/">🇰🇬 Кыргызча</a>"#));
        assert!(html.contains(r#"<a href="/tg/">🇹🇯 Тоҷикӣ</a>"#));
        assert!(html.contains(r#"<a href="/tk/">🇹🇲 Türkmençe</a>"#));
        assert!(html.contains(r#"<a href="/ps/">🇦🇫 پښتو</a>"#));
        assert!(html.contains(r#"<a href="/ckb/">🇮🇶 کوردیی ناوەندی</a>"#));
        assert!(html.contains(r#"<a href="/ku/">🌐 Kurdî</a>"#));
        assert!(html.contains(r#"<a href="/ti/">🇪🇷 ትግርኛ</a>"#));
        assert!(html.contains(r#"<a href="/rw/">🇷🇼 Kinyarwanda</a>"#));
        assert!(html.contains(r#"<a href="/mg/">🇲🇬 Malagasy</a>"#));
        assert!(html.contains(r#"<a href="/sn/">🇿🇼 chiShona</a>"#));
        assert!(html.contains(r#"<a href="/xh/">🇿🇦 isiXhosa</a>"#));
        assert!(html.contains("/v1/avatar?id=cat@hashavatar.app"));
        assert!(!html.contains("/de/v1/avatar"));
    }

    #[test]
    fn arabic_locale_renders_rtl_shell_without_reversing_urls() {
        let nonce = CspNonce("testnonce".to_string());
        let arabic = i18n(locale_by_id("ar-AE").expect("Arabic locale"));
        let html = render_index_html(&nonce, false, false, arabic);

        assert!(html.contains(r#"<html lang="ar-AE" dir="rtl">"#));
        assert!(html.contains("إنشاء أفاتار عام"));
        assert!(html.contains(r#"<div id="avatar-url" class="url-text">"#));
        assert!(html.contains("pre, code, .url-text"));
        assert!(html.contains("direction: ltr;"));
    }

    #[test]
    fn privacy_page_documents_aggregate_telemetry_limits() {
        let nonce = CspNonce("testnonce".to_string());
        let html = render_privacy_html(&nonce, false, i18n(default_locale()));

        assert!(html.contains("Privacy-Preserving Telemetry"));
        assert!(html.contains("aggregate OpenTelemetry metrics"));
        assert!(html.contains("does not include raw identifiers"));
        assert!(html.contains("IP addresses"));
        assert!(html.contains("full URLs"));
    }

    #[test]
    fn rendered_index_disables_signed_link_fetches_without_storage() {
        let nonce = CspNonce("testnonce".to_string());
        let html = render_index_html(&nonce, false, false, i18n(default_locale()));

        assert!(html.contains("const storageLinksEnabled = false;"));
        assert!(
            html.contains(r#"id="copy-signed-button" type="button" class="secondary" disabled"#)
        );
    }

    #[test]
    fn rendered_index_enables_signed_link_fetches_with_storage() {
        let nonce = CspNonce("testnonce".to_string());
        let html = render_index_html(&nonce, true, false, i18n(default_locale()));

        assert!(html.contains("const storageLinksEnabled = true;"));
        assert!(
            !html.contains(r#"id="copy-signed-button" type="button" class="secondary" disabled"#)
        );
    }

    #[test]
    fn rendered_index_exposes_avatar_style_controls() {
        let nonce = CspNonce("testnonce".to_string());
        let html = render_index_html(&nonce, false, false, i18n(default_locale()));

        assert!(html.contains(r#"<select id="accessory">"#));
        assert!(html.contains(r#"<select id="color">"#));
        assert!(html.contains(r#"<select id="expression">"#));
        assert!(html.contains(r#"<select id="shape">"#));
        assert!(html.contains(
            r#"value="cat" data-identity="cat@hashavatar.app" data-supports-layers="true""#
        ));
        assert!(html.contains(
            r#"value="planet" data-identity="planet@hashavatar.app" data-supports-layers="false""#
        ));
        assert!(html.contains(
            r#"value="bear" data-identity="bear@hashavatar.app" data-supports-layers="true""#
        ));
        assert!(html.contains(
            r#"value="coffee-cup" data-identity="coffee-cup@hashavatar.app" data-supports-layers="false""#
        ));
        for background in [
            "polka-dot",
            "striped",
            "checkerboard",
            "grid",
            "sunrise",
            "ocean",
            "starry",
        ] {
            assert!(
                html.contains(&format!(r#"value="{background}""#)),
                "missing background option {background}"
            );
        }
        assert!(html.contains("syncStyleLayerAvailability();"));
        assert!(html.contains("accessoryEl.disabled = !supportsLayers;"));
        assert!(html.contains("accessory: accessoryEl.value"));
        assert!(html.contains("color: colorEl.value"));
        assert!(html.contains("expression: expressionEl.value"));
        assert!(html.contains("shape: shapeEl.value"));
        assert!(!html.contains("algorithm-options"));
        assert!(!html.contains(r#"id="format""#));
        assert!(html.contains(r#"algorithm: "sha512""#));
        assert!(html.contains(r#"format: "webp""#));
        assert!(html.contains("id: preset.id"));
        assert!(html.contains(r#"el.addEventListener("input", scheduleFullRefresh);"#));
        assert!(html.contains("refreshNowWithPresets();"));
        assert!(!html.contains(r#"el.addEventListener("input", renderPresetPage);"#));
    }

    #[test]
    fn public_docs_do_not_advertise_metrics_as_public_api() {
        let nonce = CspNonce("testnonce".to_string());
        let index_html = render_index_html(&nonce, false, false, i18n(default_locale()));
        let docs_html = render_docs_html(&nonce, false, i18n(default_locale()));
        let openapi = openapi_document();

        assert!(!index_html.contains(r#"href="/metrics""#));
        assert!(docs_html.contains("loopback-only"));
        assert!(docs_html.contains("returns 404 to non-local peers"));
        assert!(openapi["paths"].get("/metrics").is_none());
    }

    #[test]
    fn og_png_handler_applies_avatar_rate_limits() {
        let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"));
        let handler = source
            .split("async fn og_png(")
            .nth(1)
            .and_then(|after_name| after_name.split("enum OgPngError").next())
            .expect("og_png handler should be present");

        assert!(handler.contains("enforce_limits("));
        assert!(handler.contains("RateLimitRoute::OgImage"));
        assert!(handler.contains("validate_identity(&title_id)"));
        assert!(handler.contains("tokio::task::spawn_blocking"));
        assert!(handler.contains("build_og_png_bytes("));
        assert!(handler.contains("tokio::time::timeout"));
        assert!(!handler.contains("ImageBuffer::from_pixel"));
    }

    #[test]
    fn render_json_ld_escapes_script_end_tags() {
        let nonce = CspNonce("testnonce".to_string());
        let html = render_json_ld(
            "</script><script>alert(1)</script>",
            "description",
            "https://hashavatar.app/",
            &nonce,
        );

        assert!(html.contains(r#"<\/script><script>alert(1)<\/script>"#));
        assert!(!html.contains("</script><script>alert(1)</script>"));
    }

    #[test]
    fn escape_html_attribute_handles_single_quotes() {
        assert_eq!(
            escape_html_attribute(r#"'"><tag>&"#),
            "&#39;&quot;&gt;&lt;tag&gt;&amp;"
        );
    }

    #[test]
    fn etag_uses_full_sha256_digest() {
        let etag = etag_for("example-cache-key");
        let raw = etag.trim_matches('"');

        assert_eq!(etag.len(), 66);
        assert_eq!(raw.len(), 64);
        assert!(raw.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn client_ip_ignores_forwarded_headers_from_untrusted_peers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.99"));

        let peer_ip = IpAddr::from([198, 51, 100, 10]);
        let trusted_proxies = TrustedProxies::default();

        assert_eq!(
            client_ip(&headers, peer_ip, &trusted_proxies),
            "198.51.100.10"
        );
    }

    #[test]
    fn ipv4_mapped_addresses_are_canonicalized_for_rate_limits() {
        let mapped_peer = "::ffff:198.51.100.10"
            .parse::<IpAddr>()
            .expect("mapped peer");
        let mapped_proxy = "::ffff:10.89.42.10"
            .parse::<IpAddr>()
            .expect("mapped proxy");
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("::ffff:8.8.8.8, ::ffff:10.89.42.10"),
        );
        let trusted_proxies = TrustedProxies::parse("10.89.42.0/24").expect("trusted proxy CIDR");

        assert_eq!(normalize_ip(mapped_peer).to_string(), "198.51.100.10");
        assert!(trusted_proxies.contains(mapped_proxy));
        assert_eq!(
            client_ip(&headers, mapped_proxy, &trusted_proxies),
            "8.8.8.8"
        );
        assert_eq!(
            client_ip(&HeaderMap::new(), mapped_peer, &TrustedProxies::default()),
            "198.51.100.10"
        );
    }

    #[test]
    fn client_ip_honors_forwarded_headers_from_trusted_proxies() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("8.8.8.8, 10.89.42.10"),
        );

        let peer_ip = IpAddr::from([10, 89, 42, 10]);
        let trusted_proxies = TrustedProxies::parse("10.89.42.0/24").expect("trusted proxy CIDR");

        assert_eq!(client_ip(&headers, peer_ip, &trusted_proxies), "8.8.8.8");
    }

    #[test]
    fn client_ip_uses_rightmost_untrusted_forwarded_ip() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("192.0.2.123, 8.8.8.8, 10.89.42.10"),
        );

        let peer_ip = IpAddr::from([10, 89, 42, 10]);
        let trusted_proxies = TrustedProxies::parse("10.89.42.0/24").expect("trusted proxy CIDR");

        assert_eq!(client_ip(&headers, peer_ip, &trusted_proxies), "8.8.8.8");
    }

    #[test]
    fn client_ip_rejects_reserved_forwarded_addresses_from_trusted_proxies() {
        let trusted_proxies = TrustedProxies::parse("10.89.42.0/24").expect("trusted proxy CIDR");
        let peer_ip = IpAddr::from([10, 89, 42, 10]);

        let mut headers = HeaderMap::new();
        headers.insert("cf-connecting-ip", HeaderValue::from_static("127.0.0.1"));
        headers.insert("x-real-ip", HeaderValue::from_static("10.0.0.1"));
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("192.168.1.10, 203.0.113.99, 10.89.42.10"),
        );

        assert_eq!(
            client_ip(&headers, peer_ip, &trusted_proxies),
            "10.89.42.10"
        );
    }

    #[test]
    fn client_ip_falls_back_to_peer_when_trusted_header_is_invalid() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-connecting-ip", HeaderValue::from_static("not an ip"));

        let peer_ip = IpAddr::from([10, 89, 42, 10]);
        let trusted_proxies = TrustedProxies::parse("10.89.42.0/24").expect("trusted proxy CIDR");

        assert_eq!(
            client_ip(&headers, peer_ip, &trusted_proxies),
            "10.89.42.10"
        );
    }

    #[test]
    fn invalid_algorithm_error_does_not_reflect_input() {
        let reflected = "sha512<script>alert(1)</script>";
        let error = AvatarRequest::from_query(AvatarQuery {
            algorithm: Some(reflected.to_string()),
            id: Some(DEFAULT_ID.to_string()),
            kind: None,
            background: None,
            accessory: None,
            color: None,
            expression: None,
            shape: None,
            format: None,
            size: None,
            tenant: None,
            style_version: None,
            persist: None,
            redirect: None,
        })
        .expect_err("invalid algorithm should be rejected");

        assert_eq!(error, INVALID_HASH_ALGORITHM_MESSAGE);
        assert!(!error.contains(reflected));
        assert!(!error.contains("<script>"));
    }

    #[tokio::test]
    async fn og_namespace_error_does_not_reflect_input() {
        let reflected = "public<script>alert(1)</script>";
        let state = AppState {
            storage: None,
            trusted_proxies: TrustedProxies::default(),
            rate_limiter: RateLimiter::with_capacity(8),
            metrics: Metrics::default(),
            observability: Observability::disabled(),
        };
        let response = og_png(
            State(state),
            ConnectInfo("127.0.0.1:8080".parse().expect("peer address")),
            HeaderMap::new(),
            Query(OgQuery {
                id: Some(DEFAULT_ID.to_string()),
                tenant: Some(reflected.to_string()),
                style_version: Some(DEFAULT_NAMESPACE_STYLE.to_string()),
                kind: None,
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response_text(response).await;

        assert_eq!(body, INVALID_NAMESPACE_MESSAGE);
        assert!(!body.contains(reflected));
        assert!(!body.contains("<script>"));
    }

    #[tokio::test]
    async fn og_png_rejects_oversized_identity_before_rendering() {
        let state = AppState {
            storage: None,
            trusted_proxies: TrustedProxies::default(),
            rate_limiter: RateLimiter::with_capacity(8),
            metrics: Metrics::default(),
            observability: Observability::disabled(),
        };
        let response = og_png(
            State(state),
            ConnectInfo("127.0.0.1:8080".parse().expect("peer address")),
            HeaderMap::new(),
            Query(OgQuery {
                id: Some("x".repeat(MAX_ID_BYTES + 1)),
                tenant: None,
                style_version: None,
                kind: None,
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response_text(response).await;
        assert!(body.contains("identity must be at most"));
    }

    #[tokio::test]
    async fn persisted_avatar_requests_use_storage_rate_limit() {
        let state = AppState {
            storage: None,
            trusted_proxies: TrustedProxies::default(),
            rate_limiter: RateLimiter::with_capacity(64),
            metrics: Metrics::default(),
            observability: Observability::disabled(),
        };
        let headers = HeaderMap::new();
        let peer_addr: SocketAddr = "127.0.0.1:8080".parse().expect("peer address");

        for _ in 0..RateLimitRoute::StorageLink.limit() {
            assert!(
                enforce_limits(
                    &state,
                    &headers,
                    peer_addr.ip(),
                    RateLimitRoute::StorageLink
                )
                .await
                .is_ok()
            );
        }

        let response = query_avatar(
            State(state),
            ConnectInfo(peer_addr),
            headers,
            Query(AvatarQuery {
                algorithm: None,
                id: Some(DEFAULT_ID.to_string()),
                kind: None,
                background: None,
                accessory: None,
                color: None,
                expression: None,
                shape: None,
                format: None,
                size: None,
                tenant: None,
                style_version: None,
                persist: Some(true),
                redirect: None,
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(response.headers().contains_key(header::RETRY_AFTER));
    }

    #[test]
    fn avatar_request_debug_redacts_identity() {
        let mut request = test_avatar_request(AvatarRequestFormat::Webp);
        request.identity = "user@example.com".to_string();

        let debug = format!("{request:?}");

        assert!(debug.contains("identity: \"[redacted]\""));
        assert!(!debug.contains("user@example.com"));
    }

    #[test]
    fn draw_circle_uses_wide_arithmetic_for_large_radius() {
        assert!(is_inside_circle(46_341, 0, 46_341));
        assert!(is_inside_circle(46_341, 46_341, 65_537));
        assert!(!is_inside_circle(46_341, 46_341, 46_341));
        assert!(!is_inside_circle(0, 0, -1));
    }

    #[test]
    fn overlay_reports_out_of_bounds_composition() {
        let mut canvas = RgbaImage::from_pixel(16, 16, Rgba([0, 0, 0, 0]));
        let avatar = RgbaImage::from_pixel(8, 8, Rgba([255, 255, 255, 255]));

        assert!(overlay(&mut canvas, &avatar, 4, 4).is_ok());
        assert!(overlay(&mut canvas, &avatar, 12, 12).is_err());
    }

    #[tokio::test]
    async fn internal_error_does_not_expose_details() {
        let response = internal_error("s3 bucket hashavatar-private in eu-north-1 denied");

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = response_text(response).await;

        assert_eq!(body, INTERNAL_ERROR_MESSAGE);
        assert!(!body.contains("hashavatar-private"));
        assert!(!body.contains("eu-north-1"));
    }

    #[test]
    fn build_avatar_asset_renders_webp_with_hashavatar_1_1_0() {
        let request = test_avatar_request(AvatarRequestFormat::Webp);
        let asset = build_avatar_asset(&request).expect("webp avatar should render");

        assert_eq!(asset.content_type, "image/webp");
        assert!(asset.body.starts_with(b"RIFF"));
    }

    #[test]
    fn cache_key_hashes_identity_boundaries() {
        let mut request = test_avatar_request(AvatarRequestFormat::Webp);
        request.identity = "user:cat:themed:webp".to_string();
        let asset = build_avatar_asset(&request).expect("avatar should render");

        assert!(
            asset
                .cache_key
                .contains(&sha256_hex("user:cat:themed:webp"))
        );
        assert!(!asset.cache_key.contains("user:cat:themed:webp"));
    }

    #[test]
    fn avatar_request_rejects_non_sha512_algorithm() {
        let error = AvatarRequest::from_query(AvatarQuery {
            algorithm: Some("blake3".to_string()),
            id: Some(DEFAULT_ID.to_string()),
            kind: None,
            background: None,
            accessory: None,
            color: None,
            expression: None,
            shape: None,
            format: None,
            size: None,
            tenant: None,
            style_version: None,
            persist: None,
            redirect: None,
        })
        .expect_err("non-sha512 algorithm should be rejected");

        assert_eq!(error, INVALID_HASH_ALGORITHM_MESSAGE);
    }

    #[test]
    fn avatar_request_rejects_non_webp_format() {
        let error = AvatarRequest::from_query(AvatarQuery {
            algorithm: None,
            id: Some(DEFAULT_ID.to_string()),
            kind: None,
            background: None,
            accessory: None,
            color: None,
            expression: None,
            shape: None,
            format: Some("svg".to_string()),
            size: None,
            tenant: None,
            style_version: None,
            persist: None,
            redirect: None,
        })
        .expect_err("non-webp format should be rejected");

        assert_eq!(error, INVALID_AVATAR_FORMAT_MESSAGE);
    }

    #[test]
    fn build_avatar_asset_supports_explicit_style_layers() {
        let base = test_avatar_request(AvatarRequestFormat::Webp);
        let base_asset = build_avatar_asset(&base).expect("base avatar should render");

        let mut request = base;
        request.accessory = AvatarAccessory::Glasses;
        request.color = AvatarColor::Gold;
        request.expression = AvatarExpression::Happy;
        request.shape = AvatarShape::Circle;

        let styled_asset = build_avatar_asset(&request).expect("styled avatar should render");

        assert_eq!(styled_asset.content_type, "image/webp");
        assert_ne!(base_asset.cache_key, styled_asset.cache_key);
        assert_ne!(base_asset.object_key, styled_asset.object_key);
        assert!(
            styled_asset
                .object_key
                .contains("/glasses/gold/happy/circle/")
        );
    }

    #[test]
    fn build_avatar_asset_normalizes_unsupported_accessory_layers() {
        let mut unsupported = test_avatar_request(AvatarRequestFormat::Webp);
        unsupported.kind = AvatarKind::CoffeeCup;
        unsupported.accessory = AvatarAccessory::Glasses;
        unsupported.color = AvatarColor::Gold;
        unsupported.expression = AvatarExpression::Happy;
        unsupported.shape = AvatarShape::Circle;

        let mut normalized = unsupported.clone();
        normalized.accessory = DEFAULT_ACCESSORY;
        normalized.expression = DEFAULT_EXPRESSION;

        let unsupported_asset =
            build_avatar_asset(&unsupported).expect("unsupported style avatar should render");
        let normalized_asset =
            build_avatar_asset(&normalized).expect("normalized style avatar should render");

        assert_eq!(unsupported_asset.cache_key, normalized_asset.cache_key);
        assert_eq!(unsupported_asset.object_key, normalized_asset.object_key);
        assert!(
            unsupported_asset
                .object_key
                .contains("/coffee-cup/themed/none/gold/default/circle/")
        );
    }

    #[test]
    fn build_avatar_asset_rejects_oversized_namespace() {
        let mut request = test_avatar_request(AvatarRequestFormat::Webp);
        request.namespace_tenant = "x".repeat(MAX_NAMESPACE_COMPONENT_BYTES + 1);

        let error = match build_avatar_asset(&request) {
            Ok(_) => panic!("oversized tenant should be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("tenant must be 1-64 ASCII"));
    }

    #[test]
    fn build_avatar_asset_rejects_path_like_namespace() {
        let mut request = test_avatar_request(AvatarRequestFormat::Webp);
        request.namespace_tenant = "../admin".to_string();

        let error = match build_avatar_asset(&request) {
            Ok(_) => panic!("path-like tenant should be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("tenant must be 1-64 ASCII"));
    }

    #[test]
    fn build_avatar_asset_rejects_oversized_identity() {
        let mut request = test_avatar_request(AvatarRequestFormat::Webp);
        request.identity = "x".repeat(MAX_ID_BYTES + 1);

        let error = match build_avatar_asset(&request) {
            Ok(_) => panic!("oversized identity should be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("identity must be at most 512 bytes"));
    }

    #[test]
    fn build_avatar_asset_allows_email_identity() {
        let mut request = test_avatar_request(AvatarRequestFormat::Webp);
        request.identity = "person@example.com".to_string();

        let asset = build_avatar_asset(&request).expect("email-shaped identity should render");

        assert_eq!(asset.content_type, "image/webp");
    }

    #[test]
    fn build_avatar_asset_allows_reported_identity_inputs() {
        for identity in [
            "dsdssLOLhield@hashavatar.appdsdssdasas",
            "asjkjhsajkashjL\u{00d6}OLALALALAL",
        ] {
            let mut request = test_avatar_request(AvatarRequestFormat::Webp);
            request.identity = identity.to_string();

            let asset = build_avatar_asset(&request).expect("reported identity should render");

            assert_eq!(asset.content_type, "image/webp");
        }
    }

    #[test]
    fn object_key_uses_full_sha256_digest() {
        let request = test_avatar_request(AvatarRequestFormat::Webp);
        let asset = build_avatar_asset(&request).expect("avatar should render");
        let filename = asset
            .object_key
            .rsplit('/')
            .next()
            .expect("object key filename");
        let digest = filename
            .strip_suffix(".webp")
            .expect("webp object key suffix");

        assert_eq!(digest.len(), 64);
        assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
}
