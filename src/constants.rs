use aes::{
    Aes128,
    cipher::{Array, BlockCipherEncrypt, KeyInit},
};

/// Shared CAS login target used by both iClass and BYKC VPN mode.

pub const SSO_LOGIN_URL: &str = "https://sso.buaa.edu.cn/login";

/// WebVPN CAS service target observed from the browser login flow.

pub(crate) const WEBVPN_CAS_LOGIN_URL: &str =
    "https://sso.buaa.edu.cn/login?service=https%3A%2F%2Fd.buaa.edu.cn%2Flogin%3Fcas_login%3Dtrue";

/// iClass direct base URL.

/// iClass browser entry that redirects to a transient `loginName`.

pub const ICLASS_MY_CENTER_URL: &str = "https://iclass.buaa.edu.cn:8346/?type=jumpMyCenter";

/// Undergraduate academic portal entry points used by the schedule module.

/// Portal root. Visiting it starts the SSO round trip that issues the BYXT
/// session cookie; `index.html` below is only usable as a Referer afterwards.

pub const BYXT_ROOT_URL: &str = "https://byxt.buaa.edu.cn/";

pub const BYXT_HOME_URL: &str = "https://byxt.buaa.edu.cn/jwapp/sys/homeapp/index.html";

pub const BYXT_CURRENT_USER_URL: &str =
    "https://byxt.buaa.edu.cn/jwapp/sys/homeapp/api/home/currentUser.do";

pub const BYXT_TERMS_URL: &str =
    "https://byxt.buaa.edu.cn/jwapp/sys/homeapp/api/home/student/schoolCalendars.do";

pub const BYXT_WEEKS_URL: &str =
    "https://byxt.buaa.edu.cn/jwapp/sys/homeapp/api/home/getTermWeeks.do";

pub const BYXT_WEEK_URL: &str =
    "https://byxt.buaa.edu.cn/jwapp/sys/homeapp/api/home/student/getMyScheduleDetail.do";

pub const BYXT_EXAMS_URL: &str =
    "https://byxt.buaa.edu.cn/jwapp/sys/homeapp/api/home/student/exams.do";

/// Read-only academic services hosted by the BUAA mobile portal.

pub const BUAA_SCORE_URL: &str = "https://app.buaa.edu.cn/buaascore/wap/default/index";

pub const BUAA_CLASSROOM_QUERY_URL: &str =
    "https://app.buaa.edu.cn/buaafreeclass/wap/default/search1";

pub const BUAA_CLASSROOM_REFERRER: &str = "https://app.buaa.edu.cn/site/classRoomQuery/index";

pub const BUAA_CLASSROOM_SYNC_URL: &str =
    "https://sso.buaa.edu.cn/login?service=https%3A%2F%2Fapp.buaa.edu.cn%2Fa_buaa%2Fapi%2Fcas%2Findex%3Fredirect%3Dhttps%253A%252F%252Fapp.buaa.edu.cn%252Fsite%252FclassRoomQuery%252Findex%26from%3Dwap%26login_from%3D&noAutoRedirect=1";

/// Graduate GSMIS timetable entry points used by the schedule module.

pub const GSMIS_HOME_URL: &str = "https://gsmis.buaa.edu.cn/gsapp/sys/wdkbapp/*default/index.do";

pub const GSMIS_TERMS_URL: &str =
    "https://gsmis.buaa.edu.cn/gsapp/sys/wdkbapp/modules/xskcb/kfdxnxqcx.do";

pub const GSMIS_SCHEDULE_URL: &str =
    "https://gsmis.buaa.edu.cn/gsapp/sys/wdkbapp/bykb/loadXskbData.do";

/// BYKC direct base URL.

pub const BYKC_DIRECT_BASE: &str = "https://bykc.buaa.edu.cn";

/// BYKC RSA public key used by the web client envelope.

pub const BYKC_RSA_PUBLIC_KEY_BASE64: &str =
    "MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDlHMQ3B5GsWnCe7Nlo1YiG/\
     YmHdlOiKOST5aRm4iaqYSvhvWmwcigoyWTM+8bv2+sf6nQBRDWTY4KmNV7DBk1eDnTIQo6ENA31k5/\
     tYCLEXgjPbEjCK9spiyB62fCT6cqOhbamJB0lcDJRO6Vo1m3dy+fD0jbxfDVBBNtyltIsDQIDAQAB";

/// Allowed BYKC AES session key characters.

pub const BYKC_KEY_CHARS: &[u8] = b"ABCDEFGHJKMNPQRSTWXYZabcdefhijkmnprstwxyz2345678";

/// Default BYKC page size used by paged course queries.

pub const BYKC_PAGE_SIZE: usize = 100;

#[allow(unused)]

pub const VPN_OFFSET_CORRECTION_MS: i64 = -1000;

#[derive(Clone, Copy)]

struct RawNetworkUrls {
    my_center:               &'static str,
    user_login:              &'static str,
    course_list:             &'static str,
    semester_list:           &'static str,
    course_sign_detail:      &'static str,
    sign_timestamp:          &'static str,
    scan_sign:               &'static str,
    course_schedule_by_date: &'static str,
}

#[derive(Clone, Debug)]

pub struct NetworkUrls {
    pub my_center:               String,
    pub user_login:              String,
    pub course_list:             String,
    pub semester_list:           String,
    pub course_sign_detail:      String,
    pub sign_timestamp:          String,
    pub scan_sign:               String,
    pub course_schedule_by_date: String,
}

fn raw_network_urls() -> RawNetworkUrls {

    RawNetworkUrls {
        my_center:               ICLASS_MY_CENTER_URL,
        user_login:              "https://iclass.buaa.edu.cn:8347/app/user/login.action",
        course_list:
            "https://iclass.buaa.edu.cn:8347/app/choosecourse/get_myall_course.action",
        semester_list:
            "https://iclass.buaa.edu.cn:8347/app/course/get_base_school_year.action",
        course_sign_detail:
            "https://iclass.buaa.edu.cn:8347/app/my/get_my_course_sign_detail.action",
        sign_timestamp:          "http://iclass.buaa.edu.cn:8081/app/common/get_timestamp.action",
        scan_sign:               "http://iclass.buaa.edu.cn:8081/app/course/stu_scan_sign.action",
        course_schedule_by_date:
            "https://iclass.buaa.edu.cn:8347/app/course/get_stu_course_sched.action",
    }
}

pub fn sso_vpn_entry() -> String {

    to_webvpn_url(SSO_LOGIN_URL)
}

pub fn sso_login_entry(use_vpn: bool) -> String {

    if use_vpn {

        sso_vpn_entry()
    } else {

        SSO_LOGIN_URL.to_string()
    }
}

pub fn network_urls(use_vpn: bool) -> NetworkUrls {

    let raw = raw_network_urls();

    if use_vpn {

        NetworkUrls {
            my_center:               to_webvpn_url(raw.my_center),
            user_login:              to_webvpn_url(raw.user_login),
            course_list:             to_webvpn_url(raw.course_list),
            semester_list:           to_webvpn_url(raw.semester_list),
            course_sign_detail:      to_webvpn_url(raw.course_sign_detail),
            sign_timestamp:          to_webvpn_url(raw.sign_timestamp),
            scan_sign:               to_webvpn_url(raw.scan_sign),
            course_schedule_by_date: to_webvpn_url(raw.course_schedule_by_date),
        }
    } else {

        NetworkUrls {
            my_center:               raw.my_center.to_string(),
            user_login:              raw.user_login.to_string(),
            course_list:             raw.course_list.to_string(),
            semester_list:           raw.semester_list.to_string(),
            course_sign_detail:      raw.course_sign_detail.to_string(),
            sign_timestamp:          raw.sign_timestamp.to_string(),
            scan_sign:               raw.scan_sign.to_string(),
            course_schedule_by_date: raw.course_schedule_by_date.to_string(),
        }
    }
}

pub(crate) fn to_webvpn_url(raw_url: &str) -> String {

    let Ok(parsed) = reqwest::Url::parse(raw_url) else {

        return raw_url.to_string();
    };

    if parsed.host_str() == Some("d.buaa.edu.cn") {

        return raw_url.to_string();
    }

    let Some(host) = parsed.host_str() else {

        return raw_url.to_string();
    };

    let protocol = match parsed.port() {
        None => parsed.scheme().to_string(),
        Some(80) if parsed.scheme() == "http" => "http".to_string(),
        Some(443) if parsed.scheme() == "https" => "https".to_string(),
        Some(port) => format!("{}-{}", parsed.scheme(), port),
    };

    // Use the *encoded* forms and keep the path verbatim.
    //
    // Why:
    // `Url::path()` and `query()` return decoded values, so rebuilding a URL
    // from them re-encodes the handler's input and can change what the upstream
    // sees. The path must also keep its exact shape — a trailing slash matters,
    // and dropping it sends the request somewhere different. Upstream notes the
    // same class of bug: rebuilding a path segment by segment turned `/web/`
    // into `/web`, and the service then never accepted the login callback.
    // `Url::path` already returns the percent-encoded path and preserves a
    // trailing slash, so it is used verbatim. Rebuilding the path segment by
    // segment would lose that slash, which upstream hit: `/web/` became `/web`
    // and the handler matching the exact path stopped responding.
    //
    // The query is taken from the raw string rather than `parsed.query()`
    // because SSO returns the token inside the fragment, where a `?` follows.
    // Reading the query through the parser's own view is correct for `url`,
    // but taking it from the raw text before `#` keeps the intent explicit and
    // holds if the parser's behaviour ever differs.
    let tail = format!(
        "{}{}{}",
        parsed.path(),
        query_part(raw_url),
        fragment_part(&parsed)
    );

    format!(
        "https://d.buaa.edu.cn/{}/{encrypted}{tail}",
        protocol,
        encrypted = webvpn_encrypt_host(host)
    )
}

/// The real query string, taken from before any fragment.
///
/// Why:
/// A fragment can itself contain a `?` and parameters — SSO hands tokens back
/// that way. Reading the query through the URL parser's view of the fragment
/// duplicates that token into the request query, where it does not belong.

fn query_part(raw_url: &str) -> String {

    let before_fragment = raw_url.split('#').next().unwrap_or(raw_url);

    match before_fragment.split_once('?') {
        Some((_, query)) if !query.is_empty() => format!("?{query}"),

        _ => String::new(),
    }
}

/// The fragment, including its `#`, when present.

fn fragment_part(parsed: &reqwest::Url) -> String {

    parsed
        .fragment()
        .map(|fragment| format!("#{fragment}"))
        .unwrap_or_default()
}

fn webvpn_encrypt_host(host: &str) -> String {

    const KEY: &[u8; 16] = b"wrdvpnisthebest!";

    let plain = host.as_bytes();

    let padded_len = plain.len().next_multiple_of(16);

    let mut padded = vec![b'0'; padded_len];

    padded[..plain.len()].copy_from_slice(plain);

    let key = Array::from(*KEY);

    let cipher = Aes128::new(&key);

    let mut feedback = *KEY;

    let mut ciphertext = Vec::with_capacity(padded.len());

    for block in padded.chunks(16) {

        let mut stream = Array::from(feedback);

        cipher.encrypt_block(&mut stream);

        let mut encrypted_block = [0_u8; 16];

        for (index, value) in block.iter().enumerate() {

            encrypted_block[index] = value ^ stream[index];
        }

        ciphertext.extend_from_slice(&encrypted_block);

        feedback = encrypted_block;
    }

    let mut output = hex_encode(KEY);

    output.push_str(&hex_encode(&ciphertext)[..plain.len() * 2]);

    output
}

fn hex_encode(bytes: &[u8]) -> String {

    let mut output = String::with_capacity(bytes.len() * 2);

    for byte in bytes {

        output.push_str(&format!("{byte:02x}"));
    }

    output
}

#[cfg(test)]

mod tests {

    use super::{WEBVPN_CAS_LOGIN_URL, network_urls, sso_vpn_entry, to_webvpn_url};

    #[test]

    fn webvpn_url_matches_buaa_reference_shape() {

        assert_eq!(
            sso_vpn_entry(),
            "https://d.buaa.edu.cn/https/77726476706e69737468656265737421e3e44ed225256951300d8db9d6562d/login"
        );

        assert_eq!(
            to_webvpn_url(WEBVPN_CAS_LOGIN_URL),
            "https://d.buaa.edu.cn/https/77726476706e69737468656265737421e3e44ed225256951300d8db9d6562d/login?service=https%3A%2F%2Fd.buaa.edu.cn%2Flogin%3Fcas_login%3Dtrue"
        );

        assert_eq!(
            to_webvpn_url("https://iclass.buaa.edu.cn:8347/app/user/login.action"),
            "https://d.buaa.edu.cn/https-8347/77726476706e69737468656265737421f9f44d9d342326526b0988e29d51367ba018/app/user/login.action"
        );

        assert_eq!(
            network_urls(true).my_center,
            "https://d.buaa.edu.cn/https-8346/77726476706e69737468656265737421f9f44d9d342326526b0988e29d51367ba018/?type=jumpMyCenter"
        );
    }

    #[test]

    fn webvpn_url_does_not_copy_a_fragment_token_into_the_query() {

        // SSO hands the token back inside the fragment, which itself contains a
        // `?`. The token must stay in the fragment: copying it into the request
        // query as well is rejected by the service. This pins that behaviour.
        let rewritten = to_webvpn_url("http://i.buaa.edu.cn/web/#/login?type=0&token=abc123");

        let before_fragment = rewritten.split('#').next().expect("应有路径部分");

        assert!(
            !before_fragment.contains("token="),
            "片段里的 token 不应出现在请求查询中: {rewritten}"
        );

        assert!(
            rewritten.ends_with("#/login?type=0&token=abc123"),
            "片段应原样保留: {rewritten}"
        );
    }

    #[test]

    fn webvpn_url_keeps_a_trailing_slash() {

        // A trailing slash is meaningful: upstream turned `/web/` into `/web`
        // and the handler that matched the exact path stopped responding, which
        // caused a login loop. This pins that a trailing slash survives.
        let rewritten = to_webvpn_url("http://i.buaa.edu.cn/web/");

        assert!(rewritten.ends_with("/web/"), "尾斜杠必须保留: {rewritten}");
    }

    #[test]

    fn webvpn_url_preserves_query_fragment_and_port_mapping() {

        let https_with_query = to_webvpn_url(
            "https://iclass.buaa.edu.cn:8347/app/course/list.action?x=1&name=a%20b#section",
        );

        assert!(
            https_with_query
                .starts_with("https://d.buaa.edu.cn/https-8347/77726476706e69737468656265737421")
        );

        assert!(https_with_query.ends_with("/app/course/list.action?x=1&name=a%20b#section"));

        let http_default_port = to_webvpn_url("http://iclass.buaa.edu.cn:80/app/ping?ok=1");

        assert!(
            http_default_port
                .starts_with("https://d.buaa.edu.cn/http/77726476706e69737468656265737421")
        );

        assert!(http_default_port.ends_with("/app/ping?ok=1"));

        let http_custom_port =
            to_webvpn_url("http://iclass.buaa.edu.cn:8081/app/common/get_timestamp.action");

        assert!(
            http_custom_port
                .starts_with("https://d.buaa.edu.cn/http-8081/77726476706e69737468656265737421")
        );

        assert!(http_custom_port.ends_with("/app/common/get_timestamp.action"));
    }
}
