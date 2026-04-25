/// Shared CAS login entry used by both iClass and BYKC VPN mode.
pub const SSO_VPN_ENTRY: &str = "https://d.buaa.edu.cn/";

/// iClass VPN base URL.
const VPN_BASE: &str = "https://d.buaa.edu.cn/https-8347/77726476706e69737468656265737421f9f44d9d342326526b0988e29d51367ba018";
/// iClass direct base URL.
const DIRECT_BASE: &str = "https://iclass.buaa.edu.cn:8347";

/// BYKC direct base URL.
pub const BYKC_DIRECT_BASE: &str = "https://bykc.buaa.edu.cn";
/// BYKC VPN base URL.
pub const BYKC_VPN_BASE: &str =
    "https://d.buaa.edu.cn/https/77726476706e69737468656265737421f2ee4a9f69327d517f468ca88d1b203b";
/// BYKC RSA public key used by the web client envelope.
pub const BYKC_RSA_PUBLIC_KEY_BASE64: &str = "MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDlHMQ3B5GsWnCe7Nlo1YiG/YmHdlOiKOST5aRm4iaqYSvhvWmwcigoyWTM+8bv2+sf6nQBRDWTY4KmNV7DBk1eDnTIQo6ENA31k5/tYCLEXgjPbEjCK9spiyB62fCT6cqOhbamJB0lcDJRO6Vo1m3dy+fD0jbxfDVBBNtyltIsDQIDAQAB";
/// Allowed BYKC AES session key characters.
pub const BYKC_KEY_CHARS: &[u8] = b"ABCDEFGHJKMNPQRSTWXYZabcdefhijkmnprstwxyz2345678";
/// Default BYKC page size used by paged course queries.
pub const BYKC_PAGE_SIZE: usize = 100;

#[allow(unused)]
pub const VPN_OFFSET_CORRECTION_MS: i64 = -1000;

#[derive(Clone, Copy)]
pub struct NetworkUrls {
    pub service_home: &'static str,
    pub user_login: &'static str,
    pub course_list: &'static str,
    pub semester_list: &'static str,
    pub course_sign_detail: &'static str,
    pub scan_sign: &'static str,
    pub course_schedule_by_date: &'static str,
}

pub fn network_urls(use_vpn: bool) -> NetworkUrls {
    if use_vpn {
        NetworkUrls {
            service_home: VPN_BASE,
            user_login: "https://d.buaa.edu.cn/https-8347/77726476706e69737468656265737421f9f44d9d342326526b0988e29d51367ba018/app/user/login.action",
            course_list: "https://d.buaa.edu.cn/https-8347/77726476706e69737468656265737421f9f44d9d342326526b0988e29d51367ba018/app/choosecourse/get_myall_course.action",
            semester_list: "https://d.buaa.edu.cn/https-8347/77726476706e69737468656265737421f9f44d9d342326526b0988e29d51367ba018/app/course/get_base_school_year.action",
            course_sign_detail: "https://d.buaa.edu.cn/https-8347/77726476706e69737468656265737421f9f44d9d342326526b0988e29d51367ba018/app/my/get_my_course_sign_detail.action",
            scan_sign: "https://d.buaa.edu.cn/https-8347/77726476706e69737468656265737421f9f44d9d342326526b0988e29d51367ba018/app/course/stu_scan_sign.action",
            course_schedule_by_date: "https://d.buaa.edu.cn/https-8347/77726476706e69737468656265737421f9f44d9d342326526b0988e29d51367ba018/app/course/get_stu_course_sched.action",
        }
    } else {
        NetworkUrls {
            service_home: DIRECT_BASE,
            user_login: "https://iclass.buaa.edu.cn:8347/app/user/login.action",
            course_list: "https://iclass.buaa.edu.cn:8347/app/choosecourse/get_myall_course.action",
            semester_list: "https://iclass.buaa.edu.cn:8347/app/course/get_base_school_year.action",
            course_sign_detail: "https://iclass.buaa.edu.cn:8347/app/my/get_my_course_sign_detail.action",
            scan_sign: "http://iclass.buaa.edu.cn:8081/app/course/stu_scan_sign.action",
            course_schedule_by_date: "https://iclass.buaa.edu.cn:8347/app/course/get_stu_course_sched.action",
        }
    }
}
