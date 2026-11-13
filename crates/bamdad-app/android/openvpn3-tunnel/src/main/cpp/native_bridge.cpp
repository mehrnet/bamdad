// JNI bridge between Bamdad's OpenVpnService and the OpenVPN 3 core.
//
// SPDX-License-Identifier: GPL-3.0-only
//
// Uses the OpenVPN 3 Core Library (MPL-2.0), pinned at release/3.11.8.

#include "native_bridge.hpp"

#include <atomic>
#include <cstdint>
#include <mutex>
#include <sstream>
#include <string>
#include <thread>
#include <vector>

#include <ovpncli.hpp>

#include <openvpn/client/dns_options.hpp>
#include <openvpn/tun/builder/base.hpp>
#include <openvpn/tun/builder/rgwflags.hpp>

namespace bamdad {
namespace ovpn3 {

using openvpn::ClientAPI::AppCustomControlMessageEvent;
using openvpn::ClientAPI::Config;
using openvpn::ClientAPI::EvalConfig;
using openvpn::ClientAPI::Event;
using openvpn::ClientAPI::ExternalPKICertRequest;
using openvpn::ClientAPI::ExternalPKISignRequest;
using openvpn::ClientAPI::LogInfo;
using openvpn::ClientAPI::OpenVPNClient;
using openvpn::ClientAPI::ProvideCreds;
using openvpn::ClientAPI::Status;
using openvpn::TunBuilderBase;

static JavaVM *g_vm = nullptr;
static jobject g_service = nullptr;
static jclass g_service_class = nullptr;
static jmethodID g_mid_establish_tun = nullptr;
static jmethodID g_mid_protect = nullptr;
static jmethodID g_mid_teardown = nullptr;
static jmethodID g_mid_event = nullptr;
static jmethodID g_mid_log = nullptr;
static std::atomic<bool> g_stop{false};
static std::thread g_thread;
static std::mutex g_thread_mutex;

// ---------------------------------------------------------------------------
// JNI helpers
// ---------------------------------------------------------------------------
//
// All class and method lookups happen in set_vm_and_service(), on the Java
// thread that owns the service. Calling FindClass() from the OpenVPN worker
// thread would use the boot class loader, fail for app classes, and leave a
// pending ClassNotFoundException that makes the next JNI call abort the
// process.

static JNIEnv *attach() {
  JNIEnv *env = nullptr;
  if (!g_vm) return nullptr;
  if (g_vm->GetEnv(reinterpret_cast<void **>(&env), JNI_VERSION_1_6) == JNI_OK) {
    return env;
  }
  if (g_vm->AttachCurrentThread(&env, nullptr) != JNI_OK) {
    return nullptr;
  }
  return env;
}

static void clear_exception(JNIEnv *env) {
  if (env->ExceptionCheck()) {
    env->ExceptionClear();
  }
}

// OpenVPN log/event text can contain arbitrary bytes. The JNI string helpers
// require modified UTF-8, so keep only printable ASCII and cap the length.
static std::string jni_safe(const std::string &text, size_t limit = 1500) {
  std::string out;
  out.reserve(text.size() < limit ? text.size() : limit);
  for (unsigned char c : text) {
    if (out.size() >= limit) break;
    if (c == '\n' || c == '\t' || (c >= 0x20 && c <= 0x7e)) {
      out.push_back(static_cast<char>(c));
    } else {
      out.push_back('?');
    }
  }
  return out;
}

static int call_int_string(JNIEnv *env, jmethodID mid, const std::string &value) {
  if (!g_service || !mid) return -1;
  std::string safe = jni_safe(value, 32768);
  jstring jvalue = env->NewStringUTF(safe.c_str());
  if (!jvalue) {
    clear_exception(env);
    return -1;
  }
  jint result = env->CallIntMethod(g_service, mid, jvalue);
  env->DeleteLocalRef(jvalue);
  clear_exception(env);
  return result;
}

static void call_void_string(JNIEnv *env, jmethodID mid, const std::string &value) {
  if (!g_service || !mid) return;
  std::string safe = jni_safe(value);
  jstring jvalue = env->NewStringUTF(safe.c_str());
  if (!jvalue) {
    clear_exception(env);
    return;
  }
  env->CallVoidMethod(g_service, mid, jvalue);
  env->DeleteLocalRef(jvalue);
  clear_exception(env);
}

static void call_void_bool(JNIEnv *env, jmethodID mid, bool value) {
  if (!g_service || !mid) return;
  env->CallVoidMethod(g_service, mid, value ? JNI_TRUE : JNI_FALSE);
  clear_exception(env);
}

static std::string json_escape(const std::string &s) {
  std::ostringstream os;
  for (char c : s) {
    switch (c) {
      case '"': os << "\\\""; break;
      case '\\': os << "\\\\"; break;
      case '\n': os << "\\n"; break;
      case '\r': os << "\\r"; break;
      case '\t': os << "\\t"; break;
      default: os << c;
    }
  }
  return os.str();
}

// ---------------------------------------------------------------------------
// TunBuilder accumulation
// ---------------------------------------------------------------------------

struct TunSettings {
  std::string session;
  int mtu = 0;
  std::vector<std::string> addresses;
  std::vector<std::string> routes;
  std::vector<std::string> exclude_routes;
  std::vector<std::string> dns;
};

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

class Client;

static std::atomic<Client *> g_client{nullptr};

class Client final : public OpenVPNClient {
 public:
  TunSettings settings;

  bool pause_on_connection_timeout() override { return false; }

  void event(const Event &ev) override {
    std::ostringstream os;
    os << "{\"error\":" << (ev.error ? "true" : "false")
       << ",\"fatal\":" << (ev.fatal ? "true" : "false")
       << ",\"name\":\"" << json_escape(ev.name) << "\""
       << ",\"info\":\"" << json_escape(ev.info) << "\"}";
    JNIEnv *env = attach();
    if (env) call_void_string(env, g_mid_event, os.str());
  }

  void acc_event(const AppCustomControlMessageEvent &) override {}

  void log(const LogInfo &info) override {
    JNIEnv *env = attach();
    if (env) call_void_string(env, g_mid_log, info.text);
  }

  void external_pki_cert_request(ExternalPKICertRequest &) override {}
  void external_pki_sign_request(ExternalPKISignRequest &) override {}

  bool socket_protect(openvpn_io::detail::socket_type socket, std::string, bool) override {
    JNIEnv *env = attach();
    if (!env || !g_service || !g_mid_protect) return false;
    const jboolean protected_socket =
        env->CallBooleanMethod(g_service, g_mid_protect, static_cast<jint>(socket));
    clear_exception(env);
    return protected_socket == JNI_TRUE;
  }

  // TunBuilderBase overrides.
  bool tun_builder_new() override {
    settings = TunSettings();
    return true;
  }

  bool tun_builder_set_layer(int) override { return true; }

  bool tun_builder_set_remote_address(const std::string &, bool) override { return true; }

  bool tun_builder_add_address(const std::string &address, int prefix_length,
                               const std::string &gateway, bool ipv6, bool net30) override {
    std::ostringstream os;
    os << "{\"addr\":\"" << json_escape(address) << "\",\"prefix\":" << prefix_length
       << ",\"gateway\":\"" << json_escape(gateway) << "\",\"ipv6\":"
       << (ipv6 ? "true" : "false") << ",\"net30\":" << (net30 ? "true" : "false") << "}";
    settings.addresses.push_back(os.str());
    return true;
  }

  bool tun_builder_set_route_metric_default(int) override { return true; }

  bool tun_builder_reroute_gw(bool ipv4, bool ipv6, unsigned int flags) override {
    // When the core emulates excluded routes it has already emitted the
    // concrete include routes through tun_builder_add_route(); adding a
    // default route here would defeat the exclusions.
    if (flags & openvpn::RGWFlags::EmulateExcludeRoutes) {
      return true;
    }
    if (ipv4) {
      settings.routes.push_back(
          "{\"addr\":\"0.0.0.0\",\"prefix\":0,\"metric\":-1,\"ipv6\":false}");
    }
    if (ipv6) {
      settings.routes.push_back("{\"addr\":\"::\",\"prefix\":0,\"metric\":-1,\"ipv6\":true}");
    }
    return true;
  }

  bool tun_builder_add_route(const std::string &address, int prefix_length, int metric,
                             bool ipv6) override {
    std::ostringstream os;
    os << "{\"addr\":\"" << json_escape(address) << "\",\"prefix\":" << prefix_length
       << ",\"metric\":" << metric << ",\"ipv6\":" << (ipv6 ? "true" : "false") << "}";
    settings.routes.push_back(os.str());
    return true;
  }

  bool tun_builder_exclude_route(const std::string &address, int prefix_length, int,
                                 bool ipv6) override {
    std::ostringstream os;
    os << "{\"addr\":\"" << json_escape(address) << "\",\"prefix\":" << prefix_length
       << ",\"ipv6\":" << (ipv6 ? "true" : "false") << "}";
    settings.exclude_routes.push_back(os.str());
    return true;
  }

  bool tun_builder_set_dns_options(const openvpn::DnsOptions &dns) override {
    for (const auto &entry : dns.servers) {
      for (const auto &address : entry.second.addresses) {
        if (address.port == 0) {
          settings.dns.push_back(address.address);
        }
      }
    }
    return true;
  }

  bool tun_builder_set_mtu(int mtu) override {
    settings.mtu = mtu;
    return true;
  }

  bool tun_builder_set_session_name(const std::string &name) override {
    settings.session = name;
    return true;
  }

  bool tun_builder_add_proxy_bypass(const std::string &) override { return true; }
  bool tun_builder_set_proxy_auto_config_url(const std::string &) override { return true; }
  bool tun_builder_set_proxy_http(const std::string &, int) override { return true; }
  bool tun_builder_set_proxy_https(const std::string &, int) override { return true; }
  bool tun_builder_add_wins_server(const std::string &) override { return false; }
  bool tun_builder_set_allow_family(int, bool) override { return true; }
  bool tun_builder_set_allow_local_dns(bool) override { return true; }

  int tun_builder_establish() override {
    std::ostringstream os;
    os << "{\"session\":\"" << json_escape(settings.session) << "\",\"mtu\":" << settings.mtu
       << ",\"addresses\":[" << join(settings.addresses) << "],\"routes\":["
       << join(settings.routes) << "],\"exclude_routes\":[" << join(settings.exclude_routes)
       << "],\"dns\":[" << join_strings(settings.dns) << "]}";
    JNIEnv *env = attach();
    if (!env) return -1;
    return call_int_string(env, g_mid_establish_tun, os.str());
  }

  bool tun_builder_persist() override { return false; }
  std::vector<std::string> tun_builder_get_local_networks(bool) override { return {}; }
  void tun_builder_establish_lite() override {}

  void tun_builder_teardown(bool disconnect) override {
    JNIEnv *env = attach();
    if (env) call_void_bool(env, g_mid_teardown, disconnect);
  }

 private:
  static std::string join(const std::vector<std::string> &items) {
    std::ostringstream os;
    for (size_t i = 0; i < items.size(); ++i) {
      if (i) os << ",";
      os << items[i];
    }
    return os.str();
  }

  static std::string join_strings(const std::vector<std::string> &items) {
    std::ostringstream os;
    for (size_t i = 0; i < items.size(); ++i) {
      if (i) os << ",";
      os << "\"" << json_escape(items[i]) << "\"";
    }
    return os.str();
  }
};

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

static void emit_fatal(const std::string &name, const std::string &info) {
  JNIEnv *env = attach();
  if (!env) return;
  std::ostringstream os;
  os << "{\"error\":true,\"fatal\":true,\"name\":\"" << json_escape(name)
     << "\",\"info\":\"" << json_escape(info) << "\"}";
  call_void_string(env, g_mid_event, os.str());
}

static void run_connect(const std::string &profile, const std::string &username,
                        const std::string &password, const std::string &private_key_password) {
  Client client;
  g_client.store(&client);

  Config config;
  config.content = profile;
  config.guiVersion = "Bamdad 1.0";
  config.compressionMode = "asym";
  config.enableLegacyAlgorithms = true;
  config.enableNonPreferredDCAlgorithms = true;
  config.allowLocalLanAccess = false;
#ifdef OPENVPN_PLATFORM_ANDROID
  config.enableRouteEmulation = true;
#endif
  config.privateKeyPassword = private_key_password;

  if (!g_stop.load()) {
    EvalConfig eval = client.eval_config(config);
    if (eval.error) {
      emit_fatal("EVAL_ERROR", eval.message);
      g_client.store(nullptr);
      return;
    }

    ProvideCreds creds;
    creds.username = username;
    creds.password = password;

    Status status = client.provide_creds(creds);
    if (status.error) {
      emit_fatal("CREDENTIALS_REJECTED", status.message);
      g_client.store(nullptr);
      return;
    }

    if (!g_stop.load()) {
      client.connect();
    }
  }

  g_client.store(nullptr);
}

void connect(const std::string &profile, const std::string &username,
             const std::string &password, const std::string &private_key_password) {
  std::lock_guard<std::mutex> lock(g_thread_mutex);
  if (g_thread.joinable()) g_thread.join();
  g_stop.store(false);
  g_thread = std::thread([profile, username, password, private_key_password]() {
    run_connect(profile, username, password, private_key_password);
    // The worker thread attached itself to the JVM via callbacks; detach so
    // the JVM does not keep the (now dead) native thread registered.
    if (g_vm) g_vm->DetachCurrentThread();
  });
}

void stop() {
  g_stop.store(true);
  Client *client = g_client.load();
  if (client) {
    client->stop();
  }
}

std::string stats() {
  Client *client = g_client.load();
  if (!client) return "{\"in\":0,\"out\":0}";
  const openvpn::ClientAPI::InterfaceStats stats = client->tun_stats();
  std::ostringstream os;
  os << "{\"in\":" << stats.bytesIn << ",\"out\":" << stats.bytesOut << "}";
  return os.str();
}

std::string eval(const std::string &profile) {
  Client client;
  Config config;
  config.content = profile;
  config.guiVersion = "Bamdad 1.0";
  config.compressionMode = "asym";
  config.enableLegacyAlgorithms = true;
  config.enableNonPreferredDCAlgorithms = true;
  EvalConfig result = client.eval_config(config);
  std::ostringstream os;
  os << "{\"error\":" << (result.error ? "true" : "false")
     << ",\"message\":\"" << json_escape(result.message) << "\""
     << ",\"autologin\":" << (result.autologin ? "true" : "false")
     << ",\"externalPki\":" << (result.externalPki ? "true" : "false")
     << ",\"remoteHost\":\"" << json_escape(result.remoteHost) << "\""
     << ",\"remotePort\":\"" << json_escape(result.remotePort) << "\""
     << ",\"remoteProto\":\"" << json_escape(result.remoteProto) << "\"}";
  return os.str();
}

void set_vm_and_service(JavaVM *vm, jobject service) {
  g_vm = vm;
  if (!vm || !service) return;

  JNIEnv *env = nullptr;
  if (vm->GetEnv(reinterpret_cast<void **>(&env), JNI_VERSION_1_6) != JNI_OK) return;

  if (g_service) {
    env->DeleteGlobalRef(g_service);
    g_service = nullptr;
  }
  if (g_service_class) {
    env->DeleteGlobalRef(g_service_class);
    g_service_class = nullptr;
  }

  g_service = env->NewGlobalRef(service);
  jclass local_class = env->GetObjectClass(service);
  if (!local_class) {
    clear_exception(env);
    return;
  }
  g_service_class = static_cast<jclass>(env->NewGlobalRef(local_class));
  env->DeleteLocalRef(local_class);
  if (!g_service_class) return;

  g_mid_establish_tun =
      env->GetMethodID(g_service_class, "establishTun", "(Ljava/lang/String;)I");
  g_mid_protect = env->GetMethodID(g_service_class, "protect", "(I)Z");
  g_mid_teardown = env->GetMethodID(g_service_class, "teardown", "(Z)V");
  g_mid_event = env->GetMethodID(g_service_class, "onEvent", "(Ljava/lang/String;)V");
  g_mid_log = env->GetMethodID(g_service_class, "onLog", "(Ljava/lang/String;)V");
  clear_exception(env);
}

} // namespace ovpn3
} // namespace bamdad

// ---------------------------------------------------------------------------
// JNI exports
// ---------------------------------------------------------------------------

extern "C" {

JNIEXPORT jint JNICALL JNI_OnLoad(JavaVM *vm, void *) {
  bamdad::ovpn3::set_vm_and_service(vm, nullptr);
  return JNI_VERSION_1_6;
}

JNIEXPORT void JNICALL Java_com_mehrnet_bamdad_OpenVpnNative_nativeAttach(
    JNIEnv *env, jclass, jobject service) {
  JavaVM *vm = nullptr;
  env->GetJavaVM(&vm);
  bamdad::ovpn3::set_vm_and_service(vm, service);
}

JNIEXPORT void JNICALL Java_com_mehrnet_bamdad_OpenVpnNative_nativeConnect(
    JNIEnv *env, jclass, jstring profile, jstring username, jstring password,
    jstring keyPassword) {
  const char *p = profile ? env->GetStringUTFChars(profile, nullptr) : nullptr;
  const char *u = username ? env->GetStringUTFChars(username, nullptr) : nullptr;
  const char *pw = password ? env->GetStringUTFChars(password, nullptr) : nullptr;
  const char *k = keyPassword ? env->GetStringUTFChars(keyPassword, nullptr) : nullptr;
  bamdad::ovpn3::connect(p ? p : "", u ? u : "", pw ? pw : "", k ? k : "");
  if (p) env->ReleaseStringUTFChars(profile, p);
  if (u) env->ReleaseStringUTFChars(username, u);
  if (pw) env->ReleaseStringUTFChars(password, pw);
  if (k) env->ReleaseStringUTFChars(keyPassword, k);
}

JNIEXPORT void JNICALL Java_com_mehrnet_bamdad_OpenVpnNative_nativeStop(JNIEnv *, jclass) {
  bamdad::ovpn3::stop();
}

JNIEXPORT jstring JNICALL Java_com_mehrnet_bamdad_OpenVpnNative_nativeStats(JNIEnv *env,
                                                                            jclass) {
  return env->NewStringUTF(bamdad::ovpn3::stats().c_str());
}

JNIEXPORT jstring JNICALL Java_com_mehrnet_bamdad_OpenVpnNative_nativeEval(JNIEnv *env, jclass,
                                                                           jstring profile) {
  const char *p = profile ? env->GetStringUTFChars(profile, nullptr) : nullptr;
  std::string result = bamdad::ovpn3::eval(p ? p : "");
  if (p) env->ReleaseStringUTFChars(profile, p);
  return env->NewStringUTF(result.c_str());
}

} // extern "C"
