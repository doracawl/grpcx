{
  "targets": [
    {
      "target_name": "grpcx_native",
      "sources": [
        "src/binding.cc",
        "src/wire_v2.cc",
        "src/protobuf_codec.cc",
        "src/transport_shmem.cc"
      ],
      "include_dirs": [
        "<!(node -p \"require('node-addon-api').include_dir\")"
      ],
      "cflags_cc": [ "-std=c++17", "-O3", "-fexceptions" ],
      "cflags!": [ "-fno-exceptions" ],
      "cflags_cc!": [ "-fno-exceptions" ],
      "defines": [ "NODE_ADDON_API_CPP_EXCEPTIONS" ],
      "xcode_settings": {
        "GCC_ENABLE_CPP_EXCEPTIONS": "YES",
        "CLANG_CXX_LIBRARY": "libc++",
        "CLANG_CXX_LANGUAGE_STANDARD": "c++17",
        "MACOSX_DEPLOYMENT_TARGET": "11.0",
        "GCC_OPTIMIZATION_LEVEL": "3"
      },
      "msvs_settings": {
        "VCCLCompilerTool": {
          "ExceptionHandling": "1",
          "Optimization": 3
        }
      }
    }
  ]
}
