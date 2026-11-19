//! host-net：和 rd 链进同一个 libhj_host.so。M0 只导出一个版本函数；WebTransport 在 M1 任务 11 加。
use jni::objects::JClass;
use jni::sys::jstring;
use jni::JNIEnv;

#[no_mangle]
pub extern "system" fn Java_hj_Native_version(env: JNIEnv, _c: JClass) -> jstring {
    env.new_string(rd::version()).expect("new_string").into_raw()
}
