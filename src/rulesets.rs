use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct RuleSet {
    pub id: String,
    pub kind: &'static str,
    pub real_id: &'static str,
    pub display: &'static str,
    pub group: &'static str,
    pub mihomo_bytes: u64,
    pub srs_bytes: u64,
}

fn rs(
    kind: &'static str,
    real_id: &'static str,
    display: &'static str,
    group: &'static str,
    mihomo_bytes: u64,
    srs_bytes: u64,
) -> RuleSet {
    RuleSet {
        id: format!("{kind}:{real_id}"),
        kind,
        real_id,
        display,
        group,
        mihomo_bytes,
        srs_bytes,
    }
}

/// 40 条规则集，取自 INBOUND-SCOPE §11.12 的实测库。
pub fn catalog() -> Vec<RuleSet> {
    vec![
        rs("geoip", "private", "私有地址", "直连与私网", 532, 225),
        rs("geosite", "private", "私有域名", "直连与私网", 3543, 717),
        rs("geoip", "cn", "中国大陆 IP", "直连与私网", 211667, 36454),
        rs("geosite", "cn", "中国大陆域名", "直连与私网", 2274663, 440012),
        rs("geosite", "category-ads-all", "广告与恶意域名", "广告与恶意", 22836, 7373),
        rs("geosite", "category-porn", "成人内容", "广告与恶意", 146392, 43418),
        rs("geosite", "netflix", "Netflix", "流媒体", 608, 344),
        rs("geosite", "disney", "Disney+", "流媒体", 5476, 2253),
        rs("geosite", "hulu", "Hulu", "流媒体", 1037, 445),
        rs("geosite", "youtube", "YouTube", "流媒体", 3821, 1126),
        rs("geosite", "spotify", "Spotify", "流媒体", 802, 401),
        rs("geosite", "bilibili", "哔哩哔哩", "流媒体", 1106, 463),
        rs("geosite", "youku", "优酷", "流媒体", 771, 281),
        rs("geosite", "iqiyi", "爱奇艺", "流媒体", 441, 196),
        rs("geosite", "category-entertainment", "娱乐综合", "流媒体", 52797, 17717),
        rs("geosite", "openai", "OpenAI", "AI 与开发", 716, 466),
        rs("geosite", "anthropic", "Anthropic", "AI 与开发", 449, 272),
        rs("geosite", "google-gemini", "Google Gemini", "AI 与开发", 1541, 558),
        rs("geosite", "cloudflare", "Cloudflare", "AI 与开发", 1954, 890),
        rs("geosite", "github", "GitHub", "AI 与开发", 2342, 786),
        rs("geosite", "category-dev", "开发工具", "AI 与开发", 17020, 5939),
        rs("geosite", "huggingface", "Hugging Face", "AI 与开发", 63, 69),
        rs("geosite", "telegram", "Telegram", "社交", 429, 248),
        rs("geosite", "twitter", "X / Twitter", "社交", 523, 284),
        rs("geosite", "instagram", "Instagram", "社交", 1901, 720),
        rs("geosite", "facebook", "Facebook", "社交", 9524, 3072),
        rs("geosite", "discord", "Discord", "社交", 720, 408),
        rs("geosite", "whatsapp", "WhatsApp", "社交", 252, 180),
        rs("geosite", "reddit", "Reddit", "社交", 287, 181),
        rs("geosite", "tiktok", "TikTok", "社交", 1077, 500),
        rs("geosite", "category-social-media-!cn", "海外社交媒体", "社交", 14772, 4915),
        rs("geosite", "category-games", "游戏综合", "游戏", 27441, 9626),
        rs("geosite", "epicgames", "Epic Games", "游戏", 719, 439),
        rs("geosite", "playstation", "PlayStation", "游戏", 145, 126),
        rs("geosite", "nintendo", "Nintendo", "游戏", 3155, 1331),
        rs("geosite", "xbox", "Xbox", "游戏", 958, 460),
        rs("geosite", "steam", "Steam", "游戏", 1762, 818),
        rs("geosite", "1337x", "1337x", "下载与测速", 185, 114),
        rs("geoip", "bt", "BT 下载 IP", "下载与测速", 6144, 1638),
        rs("geosite", "speedtest", "测速站点", "下载与测速", 485, 346),
    ]
}

pub fn mihomo_url(rs: &RuleSet) -> String {
    format!(
        "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/meta/geo/{}/{}.yaml",
        rs.kind, rs.real_id
    )
}

pub fn singbox_url(rs: &RuleSet) -> String {
    format!(
        "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/sing/geo/{}/{}.srs",
        rs.kind, rs.real_id
    )
}

pub fn preset_defs() -> serde_json::Value {
    serde_json::json!([
        {
            "id": "daily", "label": "日常通用", "count": 5,
            "why": "覆盖最常见的直连与广告拦截需求，体积最小",
            "items": [
                {"id": "geoip:private", "action": "direct"},
                {"id": "geosite:cn", "action": "direct"},
                {"id": "geosite:category-ads-all", "action": "reject"},
                {"id": "geosite:youtube", "action": "proxy"},
                {"id": "geosite:github", "action": "proxy"}
            ]
        },
        {
            "id": "stream", "label": "流媒体", "count": 9,
            "why": "把主流流媒体站点走代理，其余按默认分流",
            "items": [
                {"id": "geoip:private", "action": "direct"},
                {"id": "geosite:cn", "action": "direct"},
                {"id": "geosite:category-ads-all", "action": "reject"},
                {"id": "geosite:netflix", "action": "proxy"},
                {"id": "geosite:disney", "action": "proxy"},
                {"id": "geosite:youtube", "action": "proxy"},
                {"id": "geosite:hulu", "action": "proxy"},
                {"id": "geosite:spotify", "action": "proxy"},
                {"id": "geosite:category-entertainment", "action": "proxy"}
            ]
        },
        {
            "id": "ai", "label": "AI 站点", "count": 9,
            "why": "AI 与开发服务对 IP 敏感，单独分流到代理",
            "items": [
                {"id": "geoip:private", "action": "direct"},
                {"id": "geosite:cn", "action": "direct"},
                {"id": "geosite:category-ads-all", "action": "reject"},
                {"id": "geosite:openai", "action": "proxy"},
                {"id": "geosite:anthropic", "action": "proxy"},
                {"id": "geosite:google-gemini", "action": "proxy"},
                {"id": "geosite:github", "action": "proxy"},
                {"id": "geosite:huggingface", "action": "proxy"},
                {"id": "geosite:category-dev", "action": "proxy"}
            ]
        },
        {
            "id": "game", "label": "游戏", "count": 9,
            "why": "游戏平台与语音走代理，强调低延迟",
            "items": [
                {"id": "geoip:private", "action": "direct"},
                {"id": "geosite:cn", "action": "direct"},
                {"id": "geosite:category-games", "action": "proxy"},
                {"id": "geosite:steam", "action": "proxy"},
                {"id": "geosite:epicgames", "action": "proxy"},
                {"id": "geosite:playstation", "action": "proxy"},
                {"id": "geosite:nintendo", "action": "proxy"},
                {"id": "geosite:xbox", "action": "proxy"},
                {"id": "geosite:discord", "action": "proxy"}
            ]
        },
        {
            "id": "lite", "label": "低延迟低内存", "count": 5,
            "why": "只保留必要规则，减少内存与启动耗时",
            "items": [
                {"id": "geoip:private", "action": "direct"},
                {"id": "geosite:cn", "action": "direct"},
                {"id": "geosite:category-ads-all", "action": "reject"},
                {"id": "geosite:youtube", "action": "proxy"},
                {"id": "geosite:github", "action": "proxy"}
            ]
        },
        {
            "id": "privacy", "label": "仅隐私", "count": 7,
            "why": "不加流媒体规则，只做广告拦截与隐私站点代理",
            "items": [
                {"id": "geoip:private", "action": "direct"},
                {"id": "geosite:cn", "action": "direct"},
                {"id": "geosite:category-ads-all", "action": "reject"},
                {"id": "geosite:category-porn", "action": "reject"},
                {"id": "geosite:twitter", "action": "proxy"},
                {"id": "geosite:telegram", "action": "proxy"},
                {"id": "geosite:reddit", "action": "proxy"}
            ]
        }
    ])
}
