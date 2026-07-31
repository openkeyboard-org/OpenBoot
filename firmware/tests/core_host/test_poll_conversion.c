#include "openboot_transport.h"

#ifndef OB_TEST_MS
#error "test must define OB_TEST_MS"
#endif
#ifndef OB_EXPECTED_POLLS
#error "test must define OB_EXPECTED_POLLS"
#endif

_Static_assert(OB_MS_TO_POLLS(OB_TEST_MS) == OB_EXPECTED_POLLS,
               "milliseconds-to-polls conversion mismatch");
