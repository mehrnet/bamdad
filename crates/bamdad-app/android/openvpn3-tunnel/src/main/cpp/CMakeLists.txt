cmake_minimum_required(VERSION 3.22)
project(bamdad_openvpn3 CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

# Root of the vendored OpenVPN 3 Core Library (MPL-2.0).
set(OPENVPN3_SOURCE "${CMAKE_CURRENT_SOURCE_DIR}/../../../vendor/openvpn3"
    CACHE PATH "OpenVPN 3 core source tree")

if(NOT EXISTS "${OPENVPN3_SOURCE}/client/ovpncli.cpp")
  message(FATAL_ERROR
    "OpenVPN 3 sources missing at ${OPENVPN3_SOURCE}. "
    "Run: git submodule update --init --recursive")
endif()

# Static dependencies produced by scripts/build-openvpn3-deps.sh.
set(O3_DEPS "${CMAKE_CURRENT_SOURCE_DIR}/../../deps/${ANDROID_ABI}"
    CACHE PATH "OpenVPN 3 per-ABI dependency prefix")

if(NOT EXISTS "${O3_DEPS}/lib/libmbedtls.a")
  message(FATAL_ERROR
    "OpenVPN 3 dependencies for ${ANDROID_ABI} are missing at ${O3_DEPS}. "
    "Run: scripts/build-openvpn3-deps.sh ${ANDROID_ABI}")
endif()

add_library(bamdad_openvpn3 SHARED
  native_bridge.cpp
  "${OPENVPN3_SOURCE}/client/ovpncli.cpp"
  "${OPENVPN3_SOURCE}/openvpn/crypto/data_epoch.cpp"
)

target_include_directories(bamdad_openvpn3 PRIVATE
  "${CMAKE_CURRENT_SOURCE_DIR}"
  "${OPENVPN3_SOURCE}"
  "${OPENVPN3_SOURCE}/client"
  "${O3_DEPS}/include"
)

target_compile_definitions(bamdad_openvpn3 PRIVATE
  ASIO_STANDALONE
  USE_ASIO
  HAVE_LZ4
  USE_MBEDTLS
)

target_compile_options(bamdad_openvpn3 PRIVATE
  -Wall
  -Wno-unused-parameter
  -Wno-sign-compare
)

find_library(ANDROID_LOG_LIB log)

target_link_libraries(bamdad_openvpn3
  "${O3_DEPS}/lib/libmbedtls.a"
  "${O3_DEPS}/lib/libmbedx509.a"
  "${O3_DEPS}/lib/libmbedcrypto.a"
  "${O3_DEPS}/lib/liblz4.a"
  ${ANDROID_LOG_LIB}
)
