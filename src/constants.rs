use aes::{
    Aes128,
    cipher::{Array, BlockCipherEncrypt, KeyInit},
};

/// Shared CAS login target used by both iClass and BYKC VPN mode.

pub const SSO_LOGIN_URL: &str = "https://sso.buaa.edu.cn/login";

/// iClass direct base URL.

const DIRECT_BASE: &str = "https://iclass.buaa.edu.cn:8347";

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
    service_home:            &'static str,
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
    pub service_home:            String,
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
        service_home:            DIRECT_BASE,
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

pub fn network_urls(use_vpn: bool) -> NetworkUrls {

    let raw = raw_network_urls();

    if use_vpn {

        NetworkUrls {
            service_home:            to_webvpn_url(raw.service_home),
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
            service_home:            raw.service_home.to_string(),
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

    let mut tail = parsed.path().to_string();

    if let Some(query) = parsed.query() {

        tail.push('?');

        tail.push_str(query);
    }

    if let Some(fragment) = parsed.fragment() {

        tail.push('#');

        tail.push_str(fragment);
    }

    format!(
        "https://d.buaa.edu.cn/{}/{encrypted}{tail}",
        protocol,
        encrypted = webvpn_encrypt_host(host)
    )
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

    use super::{sso_vpn_entry, to_webvpn_url};

    #[test]

    fn webvpn_url_matches_buaa_reference_shape() {

        assert_eq!(
            sso_vpn_entry(),
            "https://d.buaa.edu.cn/https/77726476706e69737468656265737421e3e44ed225256951300d8db9d6562d/login"
        );

        assert_eq!(
            to_webvpn_url("https://iclass.buaa.edu.cn:8347/app/user/login.action"),
            "https://d.buaa.edu.cn/https-8347/77726476706e69737468656265737421f9f44d9d342326526b0988e29d51367ba018/app/user/login.action"
        );
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
