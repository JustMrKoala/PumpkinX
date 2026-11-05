package org.patchbukkit.testplugin.tests;

import org.bukkit.Bukkit;
import org.bukkit.Server;
import org.patchbukkit.testplugin.ConformanceTest;
import org.patchbukkit.testplugin.TestCategory;
import org.patchbukkit.testplugin.TestExpectation;

import java.util.UUID;

public final class StubTests {



    @ConformanceTest(name = "PaperLib detection classes and PaperLib.isPaper() are available", category = TestCategory.STUBS)
    public void testPaperLibDetection() throws ClassNotFoundException {
        Class.forName("com.destroystokyo.paper.PaperConfig");
        Class.forName("io.papermc.paper.configuration.Configuration");
        org.patchbukkit.testplugin.TestAssertions.assertTrue(io.papermc.lib.PaperLib.isPaper(), "PaperLib.isPaper() must be true");
    }

}
