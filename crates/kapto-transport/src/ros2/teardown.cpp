// SPDX-License-Identifier: Apache-2.0
//
// Tearing down Kapto's rcl node where a C++ exception can't reach Rust.
//
// rcl is C, but the middleware under it may be C++: rmw_zenoh_cpp can throw
// while it closes its session. An exception that unwinds out of rcl into
// Rust aborts the process ("Rust cannot catch foreign exceptions"), so the
// calls are made here, each in its own try, and teardown goes on past one
// that throws: the process is ending, and nothing can be done about it.
//
// The functions are declared with plain pointers rather than rcl's headers,
// which the build doesn't need: every argument is a pointer, and rcl_ret_t
// is a 32-bit integer.

#include <cstddef>
#include <cstdint>

extern "C" {
std::int32_t rcl_wait_set_fini(void *wait_set);
std::int32_t rcutils_uint8_array_fini(void *array);
std::int32_t rcl_subscription_fini(void *subscription, void *node);
std::int32_t rcl_node_fini(void *node);
std::int32_t rcl_shutdown(void *context);
std::int32_t rcl_context_fini(void *context);

// Finalises, in the reverse order of being made: the wait set, the
// `buffer_count` buffers (`buffer_size` bytes apart from `buffers`), the
// `subscription_count` subscriptions (`subscription_size` apart), the node,
// then the context. Returns how many of the calls threw.
std::size_t kapto_rcl_teardown(void *wait_set, char *buffers, std::size_t buffer_size,
                               std::size_t buffer_count, char *subscriptions,
                               std::size_t subscription_size, std::size_t subscription_count,
                               void *node, void *context) noexcept {
  std::size_t thrown = 0;
  auto guarded = [&thrown](auto call) {
    try {
      call();
    } catch (...) {
      thrown += 1;
    }
  };
  guarded([&] { rcl_wait_set_fini(wait_set); });
  for (std::size_t index = 0; index < buffer_count; ++index) {
    guarded([&] { rcutils_uint8_array_fini(buffers + index * buffer_size); });
  }
  for (std::size_t index = 0; index < subscription_count; ++index) {
    guarded([&] { rcl_subscription_fini(subscriptions + index * subscription_size, node); });
  }
  guarded([&] { rcl_node_fini(node); });
  guarded([&] { rcl_shutdown(context); });
  guarded([&] { rcl_context_fini(context); });
  return thrown;
}
}
