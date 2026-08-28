package com.example;

import java.util.ArrayList;
import java.util.List;

public class Main {
    public static void main(String[] args) {
        Greeter greeter = new Greeter("world");
        String message = greeter.greet();
        System.out.println(message);

        List<String> items = new ArrayList<>();
        items.add(message);
        items.size();
    }
}
