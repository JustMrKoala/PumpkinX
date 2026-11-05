package org.patchbukkit.testplugin;

import java.util.List;

public interface DynamicTestProvider {
    List<TestResult> runDynamicTests();
}
