pub const SSO_VPN_LOGIN: &str = "https://d.buaa.edu.cn/https/77726476706e69737468656265737421e3e44ed225256951300d8db9d6562d/login?service=https%3A%2F%2Fd.buaa.edu.cn%2Flogin%3Fcas_login%3Dtrue";

const VPN_BASE: &str = "https://d.buaa.edu.cn/https-8347/77726476706e69737468656265737421f9f44d9d342326526b0988e29d51367ba018";
const DIRECT_BASE: &str = "https://iclass.buaa.edu.cn:8347";

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
