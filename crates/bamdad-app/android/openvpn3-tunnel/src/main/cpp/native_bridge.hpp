#ifndef BAMDAD_OPENVPN3_NATIVE_BRIDGE_HPP
#define BAMDAD_OPENVPN3_NATIVE_BRIDGE_HPP

#include <jni.h>
#include <string>

namespace bamdad {
namespace ovpn3 {

// Set once at JNI_OnLoad. Used by the connect worker thread to call back into
// the OpenVpnService (VpnService) instance.
void set_vm_and_service(JavaVM *vm, jobject service);

// Blocks until the current session ends. Runs on a worker thread.
void connect(const std::string &profile, const std::string &username,
             const std::string &password, const std::string &private_key_password);

void stop();

// Returns a JSON object with current byte counters.
std::string stats();

// Returns a JSON object describing the profile without connecting.
std::string eval(const std::string &profile);

} // namespace ovpn3
} // namespace bamdad

#endif
